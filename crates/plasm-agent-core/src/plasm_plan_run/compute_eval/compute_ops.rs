use std::collections::BTreeMap;
use std::sync::Arc;

use minijinja::value::{Enumerator, Object, ObjectRepr};

use crate::plasm_plan::OutputName;
use crate::plasm_render_compile::render_context_hint;

use super::super::value_at_field_path as value_at_path;
use super::super::*;

pub(crate) async fn eval_compute_with_row_source(
    compute: &ComputeTemplate,
    row_source: &MaterializedRowSource,
    cross_binding_rows: &BTreeMap<String, Vec<serde_json::Value>>,
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    cgs: &CGS,
) -> Result<Vec<serde_json::Value>, String> {
    match row_source {
        MaterializedRowSource::Inline(rows) => {
            eval_compute_from_rows(compute, rows, cross_binding_rows)
        }
        MaterializedRowSource::GraphBacked {
            entity_type,
            logical_count,
            hot_snapshot,
        } => {
            if matches!(&compute.op, ComputeOp::Render { .. }) {
                let rows =
                    crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, cgs)
                        .resolve_row_source_rows(
                            row_source,
                            Some(crate::plasm_plan::PLAN_RENDER_MAX_ROWS),
                        )
                        .await?;
                return eval_compute_from_rows(compute, &rows, cross_binding_rows);
            }
            if compute_needs_full_materialize(&compute.op) {
                let rows =
                    crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, cgs)
                        .rehydrate_rows(
                            std::sync::Arc::clone(hot_snapshot),
                            entity_type,
                            *logical_count,
                        )
                        .await?;
                return eval_compute_from_rows(compute, &rows, cross_binding_rows);
            }
            eval_compute_streaming(
                compute,
                es,
                st,
                session_id,
                entity_type,
                cgs,
                std::sync::Arc::clone(hot_snapshot),
            )
            .await
        }
    }
}

pub(crate) async fn eval_compute_streaming(
    compute: &ComputeTemplate,
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    entity_type: &str,
    cgs: &CGS,
    hot_snapshot: std::sync::Arc<[plasm_runtime::CachedEntity]>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::new();
    let limit = match &compute.op {
        ComputeOp::Limit { count } => Some(*count),
        _ => None,
    };
    crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, cgs)
        .stream_entity_rows(hot_snapshot, entity_type, |row| {
            match &compute.op {
                ComputeOp::Filter { predicates } => {
                    if predicates.iter().all(|p| predicate_matches(row, p)) {
                        out.push(row.clone());
                    }
                }
                ComputeOp::Limit { .. } => out.push(row.clone()),
                ComputeOp::Project { fields } => {
                    let mut obj = serde_json::Map::new();
                    for (name, path) in fields {
                        obj.insert(
                            name.as_str().to_string(),
                            value_at_path(row, path)
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                        );
                    }
                    out.push(serde_json::Value::Object(obj));
                }
                _ => {}
            }
            limit.is_some_and(|cap| out.len() >= cap)
        })
        .await?;
    if let ComputeOp::Limit { count } = &compute.op {
        out.truncate(*count);
    }
    Ok(out)
}

pub(crate) fn eval_compute_from_rows(
    compute: &ComputeTemplate,
    rows: &[serde_json::Value],
    cross_binding_rows: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<Vec<serde_json::Value>, String> {
    match &compute.op {
        ComputeOp::Project { fields } => rows
            .iter()
            .map(|row| {
                let mut out = serde_json::Map::new();
                for (name, path) in fields {
                    out.insert(
                        name.as_str().to_string(),
                        value_at_path(row, path)
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    );
                }
                Ok(serde_json::Value::Object(out))
            })
            .collect(),
        ComputeOp::Filter { predicates } => Ok(rows
            .iter()
            .filter(|row| predicates.iter().all(|p| predicate_matches(row, p)))
            .cloned()
            .collect()),
        ComputeOp::GroupBy { keys, aggregates } => group_rows(rows, keys, aggregates),
        ComputeOp::Aggregate { aggregates } => aggregate_rows(rows, aggregates),
        ComputeOp::Sort { key, descending } => {
            let mut sorted = rows.to_vec();
            sorted
                .sort_by(|a, b| cmp_json_sort_values(value_at_path(a, key), value_at_path(b, key)));
            if *descending {
                sorted.reverse();
            }
            Ok(sorted)
        }
        ComputeOp::Limit { count } => Ok(rows.iter().take(*count).cloned().collect()),
        ComputeOp::DedupeBy { keys } => dedupe_rows(rows, keys),
        ComputeOp::Render {
            columns,
            template,
            column_aliases,
            render_bindings,
        } => render_compute(&RenderComputeInput {
            primary_rows: rows,
            columns: &RenderColumns::from_op_parts(columns.clone(), column_aliases.clone()),
            template,
            collection_alias: compute.collection_alias.as_ref(),
            render_bindings,
            binding_rows: cross_binding_rows,
        }),
    }
}
pub(crate) fn dedupe_rows(
    rows: &[serde_json::Value],
    keys: &[FieldPath],
) -> Result<Vec<serde_json::Value>, String> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let composite = if keys.is_empty() {
            serde_json::to_string(row).unwrap_or_default()
        } else {
            let parts: Vec<String> = keys
                .iter()
                .map(|k| {
                    value_at_path(row, k)
                        .map(json_scalar_display)
                        .unwrap_or_default()
                })
                .collect();
            serde_json::to_string(&parts).unwrap_or_default()
        };
        if seen.insert(composite) {
            out.push(row.clone());
        }
    }
    Ok(out)
}

pub(crate) fn group_rows(
    rows: &[serde_json::Value],
    keys: &[FieldPath],
    aggregates: &[crate::plasm_plan::AggregateSpec],
) -> Result<Vec<serde_json::Value>, String> {
    if keys.is_empty() {
        return Err("group_by requires at least one key".into());
    }
    let mut groups: BTreeMap<String, Vec<&serde_json::Value>> = BTreeMap::new();
    for row in rows {
        let parts: Vec<String> = keys
            .iter()
            .map(|k| {
                value_at_path(row, k)
                    .map(json_scalar_display)
                    .unwrap_or_default()
            })
            .collect();
        let composite = serde_json::to_string(&parts).unwrap_or_default();
        groups.entry(composite).or_default().push(row);
    }
    let mut out = Vec::new();
    for (composite, group_rows) in groups {
        let parts: Vec<String> = serde_json::from_str(&composite).unwrap_or_default();
        let mut obj = serde_json::Map::new();
        for (key_path, part) in keys.iter().zip(parts.iter()) {
            obj.insert(key_path.dotted(), serde_json::Value::String(part.clone()));
        }
        append_aggregates(&mut obj, &group_rows, aggregates)?;
        out.push(serde_json::Value::Object(obj));
    }
    Ok(out)
}

pub(crate) fn aggregate_rows(
    rows: &[serde_json::Value],
    aggregates: &[crate::plasm_plan::AggregateSpec],
) -> Result<Vec<serde_json::Value>, String> {
    let refs = rows.iter().collect::<Vec<_>>();
    let mut obj = serde_json::Map::new();
    append_aggregates(&mut obj, &refs, aggregates)?;
    Ok(vec![serde_json::Value::Object(obj)])
}

pub(crate) fn append_aggregates(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    rows: &[&serde_json::Value],
    aggregates: &[crate::plasm_plan::AggregateSpec],
) -> Result<(), String> {
    for agg in aggregates {
        let value = match agg.function {
            AggregateFunction::Count => serde_json::json!(rows.len()),
            AggregateFunction::Sum => {
                serde_json::json!(aggregate_numbers(rows, agg.field.as_ref())
                    .iter()
                    .sum::<f64>())
            }
            AggregateFunction::Avg => {
                let nums = aggregate_numbers(rows, agg.field.as_ref());
                serde_json::json!(if nums.is_empty() {
                    0.0
                } else {
                    nums.iter().sum::<f64>() / nums.len() as f64
                })
            }
            AggregateFunction::Min => aggregate_numbers(rows, agg.field.as_ref())
                .into_iter()
                .reduce(f64::min)
                .map(|n| serde_json::json!(n))
                .unwrap_or(serde_json::Value::Null),
            AggregateFunction::Max => aggregate_numbers(rows, agg.field.as_ref())
                .into_iter()
                .reduce(f64::max)
                .map(|n| serde_json::json!(n))
                .unwrap_or(serde_json::Value::Null),
            AggregateFunction::First => rows
                .first()
                .and_then(|row| {
                    agg.field
                        .as_ref()
                        .and_then(|f| value_at_path(row, f))
                        .cloned()
                })
                .unwrap_or(serde_json::Value::Null),
            AggregateFunction::Last => rows
                .last()
                .and_then(|row| {
                    agg.field
                        .as_ref()
                        .and_then(|f| value_at_path(row, f))
                        .cloned()
                })
                .unwrap_or(serde_json::Value::Null),
        };
        obj.insert(agg.name.as_str().to_string(), value);
    }
    Ok(())
}

pub(crate) fn aggregate_numbers(
    rows: &[&serde_json::Value],
    field: Option<&FieldPath>,
) -> Vec<f64> {
    rows.iter()
        .filter_map(|row| {
            field
                .and_then(|f| value_at_path(row, f))
                .and_then(json_number)
        })
        .collect()
}

pub(crate) struct RenderComputeInput<'a> {
    pub primary_rows: &'a [serde_json::Value],
    pub columns: &'a RenderColumns,
    pub template: &'a str,
    pub collection_alias: Option<&'a OutputName>,
    pub render_bindings: &'a [OutputName],
    pub binding_rows: &'a BTreeMap<String, Vec<serde_json::Value>>,
}

fn effective_render_binding_labels(
    render_bindings: &[OutputName],
    collection_alias: Option<&OutputName>,
) -> Vec<String> {
    if !render_bindings.is_empty() {
        render_bindings
            .iter()
            .map(|label| label.as_str().to_string())
            .collect()
    } else if let Some(alias) = collection_alias {
        vec![alias.as_str().to_string()]
    } else {
        vec![]
    }
}

pub(crate) fn render_compute(
    input: &RenderComputeInput<'_>,
) -> Result<Vec<serde_json::Value>, String> {
    let rows = input.primary_rows;
    if rows.len() > PLAN_RENDER_MAX_ROWS {
        return Err(format!(
            "Plan.render source has {} rows; use Plan.limit(...) to stay at or below {PLAN_RENDER_MAX_ROWS}",
            rows.len()
        ));
    }
    let projected = rows
        .iter()
        .enumerate()
        .map(|(row_index, row)| {
            input
                .columns
                .project_row(row, row_index)
                .map(serde_json::Value::Object)
        })
        .collect::<Result<Vec<_>, String>>()?;

    let mut env = minijinja::Environment::new();
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env.add_template("plan_render", input.template)
        .map_err(|e| format!("Plan.render template compile error: {e}"))?;
    let tmpl = env
        .get_template("plan_render")
        .map_err(|e| format!("Plan.render template load error: {e}"))?;
    let alias_name = input.collection_alias.map(|a| a.as_str());
    let rows_val = minijinja::Value::from_serialize(&projected);
    let mut ctx: BTreeMap<String, minijinja::Value> =
        BTreeMap::from([("rows".to_string(), rows_val)]);
    for label in effective_render_binding_labels(input.render_bindings, input.collection_alias) {
        let binding_rows = input
            .binding_rows
            .get(label.as_str())
            .map(|rows| rows.as_slice())
            .unwrap_or(rows);
        ctx.insert(label, template_binding_value(binding_rows));
    }
    let rendered = tmpl.render(ctx).map_err(|e| {
        if matches!(e.kind(), minijinja::ErrorKind::UndefinedError) {
            format!(
                "Plan.render template render error: {e}. {}",
                render_context_hint(input.columns, alias_name)
            )
        } else {
            format!("Plan.render template render error: {e}")
        }
    })?;
    if rendered.chars().count() > PLAN_RENDER_MAX_OUTPUT_CHARS {
        return Err(format!(
            "Plan.render output exceeds {PLAN_RENDER_MAX_OUTPUT_CHARS} characters"
        ));
    }

    Ok(vec![serde_json::json!({ "content": rendered })])
}

/// A render-binding value bound into the Minijinja context (collection alias / cross-binding source).
///
/// It is BOTH an iterable sequence — so `{% for r in items %}` always works, even for a single-row
/// binding — AND an object whose attribute access delegates to the first row, so the single-object
/// convenience `{{ items.title }}` still resolves. This resolves the prior collapse where a 1-row
/// binding bound as a scalar object and `{% for r in items %}` iterated an object (undefined value).
#[derive(Debug)]
struct RenderBindingValue {
    rows: Vec<minijinja::Value>,
}

impl Object for RenderBindingValue {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Seq
    }

    fn get_value(self: &Arc<Self>, key: &minijinja::Value) -> Option<minijinja::Value> {
        // Attribute access (`items.title`) delegates to the first row.
        if let Some(name) = key.as_str() {
            return self
                .rows
                .first()
                .and_then(|row| row.get_attr(name).ok())
                .filter(|value| !value.is_undefined());
        }
        // Sequence index access (`items[0]`) and `{% for … %}` iteration.
        usize::try_from(key.clone())
            .ok()
            .and_then(|idx| self.rows.get(idx).cloned())
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Seq(self.rows.len())
    }
}

fn template_binding_value(rows: &[serde_json::Value]) -> minijinja::Value {
    let rows: Vec<minijinja::Value> = rows.iter().map(minijinja::Value::from_serialize).collect();
    minijinja::Value::from_object(RenderBindingValue { rows })
}

pub(crate) fn binding_rows_for_render(
    compute: &ComputeTemplate,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<BTreeMap<String, Vec<serde_json::Value>>, String> {
    let ComputeOp::Render {
        render_bindings, ..
    } = &compute.op
    else {
        return Ok(BTreeMap::new());
    };
    let labels =
        effective_render_binding_labels(render_bindings, compute.collection_alias.as_ref());
    let mut out = BTreeMap::new();
    for label in labels {
        let node_id = PlanNodeId::new(label.clone())?;
        let mat = materialized.get(&node_id).ok_or_else(|| {
            format!("Plan.render binding `{label}`: node `{label}` has not been materialized")
        })?;
        let rows = mat
            .row_source
            .inline_rows()
            .map(|r| r.to_vec())
            .ok_or_else(|| {
                format!("Plan.render binding `{label}`: node `{label}` is not inline materialized")
            })?;
        out.insert(label, rows);
    }
    Ok(out)
}

pub(crate) fn json_number(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_i64().map(|n| n as f64))
}

pub(crate) fn json_scalar_display(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
pub(crate) fn json_plasm_literal_display(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => serde_json::to_string(s)
            .unwrap_or_else(|_| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn sort_display_key(v: Option<&serde_json::Value>) -> String {
    v.map(json_scalar_display).unwrap_or_default()
}

/// Compare two JSON cell values for deterministic `.sort(...)` ordering.
///
/// When both values are numeric (JSON numbers or strings that parse as integers/floats), ordering is
/// numeric so multi-digit values sort correctly (`87` before `300`). Otherwise ordering follows the
/// legacy string collation used by [`sort_display_key`] (including missing/`null` → empty string).
pub(crate) fn cmp_json_sort_values(
    a: Option<&serde_json::Value>,
    b: Option<&serde_json::Value>,
) -> std::cmp::Ordering {
    match (a, b) {
        (Some(va), Some(vb)) => {
            if let (Some(na), Some(nb)) = (json_number(va), json_number(vb)) {
                return na.total_cmp(&nb);
            }
            if let (Some(sa), Some(sb)) = (va.as_str(), vb.as_str()) {
                if let (Ok(ia), Ok(ib)) = (sa.parse::<i64>(), sb.parse::<i64>()) {
                    return ia.cmp(&ib);
                }
                if let (Ok(fa), Ok(fb)) = (sa.parse::<f64>(), sb.parse::<f64>()) {
                    return fa.total_cmp(&fb);
                }
            }
            sort_display_key(Some(va)).cmp(&sort_display_key(Some(vb)))
        }
        _ => sort_display_key(a).cmp(&sort_display_key(b)),
    }
}
pub(crate) fn compute_fingerprint(node: &ValidatedPlanNode, rows: &[serde_json::Value]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(node.id().as_str().as_bytes());
    if let ValidatedPlanNode::Compute(compute) = node {
        match serde_json::to_vec(&compute.compute) {
            Ok(bytes) => hasher.update(bytes),
            Err(e) => hasher.update(format!("compute-serialization-error:{e}").as_bytes()),
        }
    }
    match serde_json::to_vec(rows) {
        Ok(bytes) => hasher.update(bytes),
        Err(e) => hasher.update(format!("rows-serialization-error:{e}").as_bytes()),
    }
    format!("plan-compute:{}", hex::encode(hasher.finalize()))
}
