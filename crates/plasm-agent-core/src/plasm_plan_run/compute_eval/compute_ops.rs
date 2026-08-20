use std::collections::BTreeMap;
use std::sync::Arc;

use minijinja::value::{Enumerator, Object, ObjectRepr};
use plasm_runtime::{eval_compute_ops, ComputeEvalOutcome};

use crate::plasm_plan::OutputName;
use crate::plasm_render_compile::render_context_hint;

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
    let cap = if matches!(&compute.op, ComputeOp::Render { .. }) {
        Some(crate::plasm_plan::PLAN_RENDER_MAX_ROWS)
    } else {
        None
    };
    let rows = crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, cgs)
        .resolve_row_source_rows(row_source, cap)
        .await?;
    eval_compute_from_rows(compute, &rows, cross_binding_rows)
}

pub(crate) fn eval_compute_from_rows(
    compute: &ComputeTemplate,
    rows: &[serde_json::Value],
    cross_binding_rows: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<Vec<serde_json::Value>, String> {
    match eval_compute_ops(std::slice::from_ref(&compute.op), rows)? {
        ComputeEvalOutcome::Rows(out) => Ok(out),
        ComputeEvalOutcome::Render {
            rows,
            columns,
            column_aliases,
            template,
            collection_alias,
            render_bindings,
        } => render_compute(&RenderComputeInput {
            primary_rows: &rows,
            columns: &RenderColumns::from_op_parts(columns, column_aliases),
            template: &template,
            collection_alias: collection_alias
                .as_ref()
                .or(compute.collection_alias.as_ref()),
            render_bindings: &render_bindings,
            binding_rows: cross_binding_rows,
        }),
    }
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
        if let Some(name) = key.as_str() {
            return self
                .rows
                .first()
                .and_then(|row| row.get_attr(name).ok())
                .filter(|value| !value.is_undefined());
        }
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
