use super::super::*;
use super::compute_ops::render_compute;

pub(crate) fn materialized_singleton_inputs(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    inputs: &[ValidatedPlanDataInput],
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let mut out = BTreeMap::new();
    for input in inputs {
        let mat = materialized.get(&input.node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                input.node.as_str(),
                input.alias.as_str()
            )
        })?;
        if mat.inline_row_count() != 1 {
            return Err(singleton_input_row_count_error(
                input.node.as_str(),
                input.alias.as_str(),
                mat.inline_row_count(),
                format!("{:?} broadcast", input.proof).as_str(),
            ));
        }
        let row = mat.first_inline_row().cloned().ok_or_else(|| {
            format!(
                "Plan input {:?} for alias {:?} expected one row but was empty",
                input.node.as_str(),
                input.alias.as_str()
            )
        })?;
        out.insert(
            input.alias.clone(),
            MaterializedInputRow {
                node: input.node.clone(),
                proof: input.proof,
                row: augment_row_json_with_identity(
                    &row,
                    mat.row_identities.first().and_then(|i| i.as_ref()),
                ),
                row_identity: mat.row_identities.first().cloned().flatten(),
            },
        );
    }
    Ok(out)
}

pub(crate) fn materialized_result_use_inputs(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    uses_result: &[PlanResultUse],
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let mut out = BTreeMap::new();
    for use_result in uses_result {
        let node = PlanNodeId::new(use_result.node.clone())?;
        let alias = InputAlias::new(use_result.r#as.clone())?;
        let mat = materialized.get(&node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                node.as_str(),
                alias.as_str()
            )
        })?;
        if mat.inline_row_count() != 1 {
            return Err(singleton_input_row_count_error(
                node.as_str(),
                alias.as_str(),
                mat.inline_row_count(),
                "staged expression rendering",
            ));
        }
        let row = mat.first_inline_row().cloned().ok_or_else(|| {
            format!(
                "Plan input {:?} for alias {:?} expected one row but was empty",
                node.as_str(),
                alias.as_str()
            )
        })?;
        out.insert(
            alias,
            MaterializedInputRow {
                node,
                proof: crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton,
                row: augment_row_json_with_identity(
                    &row,
                    mat.row_identities.first().and_then(|i| i.as_ref()),
                ),
                row_identity: mat.row_identities.first().cloned().flatten(),
            },
        );
    }
    Ok(out)
}

pub(crate) fn singleton_input_row_count_error(
    node: &str,
    alias: &str,
    row_count: usize,
    context: &str,
) -> String {
    if row_count == 0 {
        format!(
            "Plan input {node:?} for alias {alias:?} expected exactly one row for {context}, but the source produced zero rows. This is a data-empty result, not a Plasm syntax error: run or inspect {node:?}, loosen filters if it should match, branch around empty results, or use `.singleton()` only when exactly one row is guaranteed."
        )
    } else {
        format!(
            "Plan input {node:?} for alias {alias:?} expected exactly one row for {context}, but the source produced {row_count} rows. Add filters/projection to make the source unique, aggregate intentionally, or use `.singleton()` only when exactly one row is guaranteed."
        )
    }
}

pub(crate) fn materialized_result_use_inputs_with_source_row(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    uses_result: &[PlanResultUse],
    source_node: &PlanNodeId,
    source_row: &serde_json::Value,
    source_row_identity: Option<plasm_core::RowIdentity>,
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let mut out = BTreeMap::new();
    for use_result in uses_result {
        let node = PlanNodeId::new(use_result.node.clone())?;
        let alias = InputAlias::new(use_result.r#as.clone())?;
        let mat = materialized.get(&node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                node.as_str(),
                alias.as_str()
            )
        })?;
        let (row, row_identity) = if node == *source_node {
            (
                augment_row_json_with_identity(source_row, source_row_identity.as_ref()),
                source_row_identity.clone(),
            )
        } else {
            if mat.inline_row_count() != 1 {
                return Err(singleton_input_row_count_error(
                    node.as_str(),
                    alias.as_str(),
                    mat.inline_row_count(),
                    "staged expression rendering",
                ));
            }
            let row = mat.first_inline_row().cloned().ok_or_else(|| {
                format!(
                    "Plan input {:?} for alias {:?} expected one row but was empty",
                    node.as_str(),
                    alias.as_str()
                )
            })?;
            (
                augment_row_json_with_identity(
                    &row,
                    mat.row_identities.first().and_then(|i| i.as_ref()),
                ),
                mat.row_identities.first().cloned().flatten(),
            )
        };
        out.insert(
            alias,
            MaterializedInputRow {
                node,
                proof: crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton,
                row,
                row_identity,
            },
        );
    }
    Ok(out)
}

pub(crate) fn instantiate_parsed_expr_plan_inputs_with_rows(
    parsed: ParsedExpr,
    input_rows: &BTreeMap<InputAlias, MaterializedInputRow>,
    wire_coercion: Option<WireCoercionCtx<'_>>,
) -> Result<ParsedExpr, String> {
    if input_rows.is_empty() {
        return Ok(parsed);
    }
    let scope = EvalScope::Root {
        row: &serde_json::Value::Null,
    };
    let inputs = InputEnv { rows: input_rows };
    let env = PlanEvalEnv {
        scope,
        inputs,
        wire_coercion,
    };
    let expr_json = serde_json::to_value(&parsed.expr)
        .map_err(|e| format!("serialize expr for hole instantiation: {e}"))?;
    let expr_json = instantiate_expr_template_value(&expr_json, &env)?;
    let expr: Expr = serde_json::from_value(expr_json)
        .map_err(|e| format!("deserialize expr after hole instantiation: {e}"))?;
    Ok(ParsedExpr {
        expr,
        projection: parsed.projection,
    })
}

/// Deserialize → [`instantiate_expr_template_value`] → deserialize so predicate/CML env holes (e.g.
/// `__plasm_hole` `node_input`) become concrete row JSON **before** HTTP compile — parity with dry-run
/// topology checks that assumed splattable scope rows.
pub(crate) fn instantiate_parsed_expr_plan_inputs(
    parsed: ParsedExpr,
    uses_result: &[PlanResultUse],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<ParsedExpr, String> {
    if uses_result.is_empty() {
        return Ok(parsed);
    }
    let input_rows = materialized_result_use_inputs(materialized, uses_result)?;
    instantiate_parsed_expr_plan_inputs_with_rows(parsed, &input_rows, None)
}

pub(crate) fn wire_coercion_ctx_for_source_entity<'a>(
    cgs: &'a CGS,
    source_entity_name: &str,
) -> Option<WireCoercionCtx<'a>> {
    let ent = cgs.get_entity(source_entity_name)?;
    Some(WireCoercionCtx {
        cgs,
        source_entity: ent,
    })
}
pub(crate) fn instantiate_expr_template(
    template: &ValidatedPlanExprTemplate,
    env: &PlanEvalEnv<'_>,
) -> Result<ParsedExpr, String> {
    let expr_json = instantiate_expr_template_value(&template.expr, env)?;
    let expr = serde_json::from_value(expr_json)
        .map_err(|e| format!("templated Plasm IR instantiation failed: {e}"))?;
    Ok(ParsedExpr {
        expr,
        projection: template.projection.clone(),
    })
}

pub(crate) fn instantiate_raw_expr_template(
    template: &PlanExprTemplate,
    env: &PlanEvalEnv<'_>,
) -> Result<ParsedExpr, String> {
    let expr_json = instantiate_expr_template_value(&template.expr, env)?;
    let expr = serde_json::from_value(expr_json)
        .map_err(|e| format!("templated Plasm IR instantiation failed: {e}"))?;
    Ok(ParsedExpr {
        expr,
        projection: template.projection.clone(),
    })
}

pub(crate) fn instantiate_expr_template_value(
    value: &serde_json::Value,
    env: &PlanEvalEnv<'_>,
) -> Result<serde_json::Value, String> {
    if let Some(hole) = value
        .as_object()
        .and_then(|obj| obj.get("__plasm_hole"))
        .and_then(|v| v.as_object())
    {
        return instantiate_ir_hole(hole, env);
    }
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| instantiate_expr_template_value(item, env))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(k, v)| Ok((k.clone(), instantiate_expr_template_value(v, env)?)))
            .collect::<Result<serde_json::Map<_, _>, String>>()
            .map(serde_json::Value::Object),
        serde_json::Value::String(s) => {
            if !plasm_core::contains_dollar_interpolation(s) {
                return Ok(serde_json::Value::String(s.clone()));
            }
            let scope = plan_binding_scope_owned(env);
            let out = plasm_core::interpolate_string_map(s, &scope)
                .map_err(|e| format!("string interpolation: {e}"))?;
            Ok(serde_json::Value::String(out))
        }
        other => Ok(other.clone()),
    }
}

pub(crate) fn plan_binding_scope_owned(
    env: &PlanEvalEnv<'_>,
) -> BTreeMap<String, plasm_core::Value> {
    let mut scope = BTreeMap::new();
    for (alias, input) in env.inputs.rows {
        let row_value = json_row_to_plasm_value(&input.row);
        scope.insert(alias.as_str().to_string(), row_value.clone());
        scope.insert(input.node.as_str().to_string(), row_value);
    }
    if let EvalScope::Bound { row, binding } = &env.scope {
        scope.insert(binding.as_str().to_string(), json_row_to_plasm_value(row));
    }
    scope
}

pub(crate) fn json_row_to_plasm_value(row: &serde_json::Value) -> plasm_core::Value {
    match row {
        serde_json::Value::Null => plasm_core::Value::Null,
        serde_json::Value::Bool(b) => plasm_core::Value::Bool(*b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(plasm_core::Value::Integer)
            .or_else(|| n.as_f64().map(plasm_core::Value::Float))
            .unwrap_or(plasm_core::Value::Null),
        serde_json::Value::String(s) => plasm_core::Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            plasm_core::Value::Array(items.iter().map(json_row_to_plasm_value).collect())
        }
        serde_json::Value::Object(map) => {
            let mut out = indexmap::IndexMap::new();
            for (k, v) in map {
                out.insert(k.clone(), json_row_to_plasm_value(v));
            }
            plasm_core::Value::Object(out)
        }
    }
}

pub(crate) fn augment_row_json_with_identity(
    row: &serde_json::Value,
    identity: Option<&plasm_core::RowIdentity>,
) -> serde_json::Value {
    let Some(identity) = identity else {
        return row.clone();
    };
    let mut obj = match row {
        serde_json::Value::Object(map) => map.clone(),
        other => {
            let mut m = serde_json::Map::new();
            m.insert("value".to_string(), other.clone());
            m
        }
    };
    let primary = identity.reference.primary_slot_str();
    obj.entry("id".to_string())
        .or_insert_with(|| serde_json::Value::String(primary.clone()));
    for (k, v) in &identity.ambient {
        obj.entry(k.clone())
            .or_insert_with(|| serde_json::Value::String(v.clone()));
    }
    if let plasm_core::EntityKey::Compound(parts) = &identity.reference.key {
        for (k, v) in parts {
            obj.entry(k.clone())
                .or_insert_with(|| serde_json::Value::String(v.clone()));
        }
    }
    serde_json::Value::Object(obj)
}

pub(crate) fn coerce_node_input_json(
    ctx: Option<&WireCoercionCtx<'_>>,
    path: &[String],
    value: serde_json::Value,
) -> serde_json::Value {
    let Some(ctx) = ctx else {
        return value;
    };
    let Some(field) = path.last().map(String::as_str) else {
        return value;
    };
    match plasm_core::parent_entity_field_type(ctx.cgs, ctx.source_entity, field) {
        Ok(ft) => {
            let nv = ctx
                .source_entity
                .fields
                .get(field)
                .and_then(|f| f.named_value(ctx.cgs).ok());
            plasm_core::coerce_json_value_for_field_type(
                &ft,
                nv.and_then(|n| n.value_format),
                nv.and_then(|n| n.array_items.as_ref()),
                value,
            )
        }
        Err(_) => value,
    }
}

pub(crate) fn node_input_hole_from_identity(
    ctx: Option<&WireCoercionCtx<'_>>,
    identity: &Option<plasm_core::RowIdentity>,
    path: &[String],
    row: &serde_json::Value,
) -> Option<serde_json::Value> {
    let identity = identity.as_ref()?;
    if path.is_empty() {
        let slot = identity.reference.primary_slot_str();
        return Some(coerce_node_input_json(
            ctx,
            path,
            serde_json::Value::String(slot),
        ));
    }
    if path.len() == 1 {
        let key = path[0].as_str();
        if key == "id" {
            let slot = identity.reference.primary_slot_str();
            return Some(coerce_node_input_json(
                ctx,
                path,
                serde_json::Value::String(slot),
            ));
        }
        if let Some(v) = identity.ambient.get(key) {
            return Some(coerce_node_input_json(
                ctx,
                path,
                serde_json::Value::String(v.clone()),
            ));
        }
        if let plasm_core::EntityKey::Compound(parts) = &identity.reference.key {
            if let Some(v) = parts.get(key) {
                let raw = ctx
                    .map(|c| plasm_core::identity_slot_to_json(c.cgs, c.source_entity, key, v))
                    .unwrap_or_else(|| serde_json::Value::String(v.clone()));
                return Some(coerce_node_input_json(ctx, path, raw));
            }
        }
    }
    value_at_segments(row, path)
        .cloned()
        .map(|v| coerce_node_input_json(ctx, path, v))
}

pub(crate) fn instantiate_ir_hole(
    hole: &serde_json::Map<String, serde_json::Value>,
    env: &PlanEvalEnv<'_>,
) -> Result<serde_json::Value, String> {
    let kind = hole
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "IR value hole is missing kind".to_string())?;
    let path = hole
        .get("path")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    match kind {
        "binding" => {
            let binding = hole
                .get("binding")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "binding IR hole is missing binding".to_string())?;
            let EvalScope::Bound {
                binding: scope_binding,
                ..
            } = &env.scope
            else {
                return Err("binding IR hole cannot be used outside a row scope".to_string());
            };
            if binding != scope_binding.as_str() {
                return Err(format!(
                    "binding IR hole references {binding:?}, but active binding is {:?}",
                    scope_binding.as_str()
                ));
            }
            Ok(value_at_segments(env.scope.row(), &path)
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        }
        "node_input" => {
            let alias = hole
                .get("alias")
                .and_then(|v| v.as_str())
                .or_else(|| hole.get("node").and_then(|v| v.as_str()))
                .ok_or_else(|| "node_input IR hole is missing alias".to_string())?;
            let alias = InputAlias::new(alias.to_string())?;
            let input = env.inputs.rows.get(&alias).ok_or_else(|| {
                format!("node_input IR hole references unavailable alias {alias:?}")
            })?;
            let from_row = value_at_segments(&input.row, &path).cloned();
            let from_row_usable = from_row
                .as_ref()
                .is_some_and(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.is_empty()));
            if from_row_usable {
                return Ok(coerce_node_input_json(
                    env.wire_coercion.as_ref(),
                    &path,
                    from_row.unwrap(),
                ));
            }
            if let Some(value) = node_input_hole_from_identity(
                env.wire_coercion.as_ref(),
                &input.row_identity,
                &path,
                &input.row,
            ) {
                if value.as_str().is_none_or(|s| !s.is_empty()) {
                    return Ok(value);
                }
            }
            Ok(from_row.unwrap_or(serde_json::Value::Null))
        }
        other => Err(format!("unknown IR value hole kind {other:?}")),
    }
}
pub(crate) fn plan_value_to_rows(value: &PlanValue) -> Result<Vec<serde_json::Value>, String> {
    let inputs = BTreeMap::new();
    let scope = EvalScope::Root {
        row: &serde_json::Value::Null,
    };
    let input_env = InputEnv { rows: &inputs };
    let env = PlanEvalEnv {
        scope,
        inputs: input_env,
        wire_coercion: None,
    };
    let json = eval_plan_value(value, &env)?;
    Ok(match json {
        serde_json::Value::Array(items) => items,
        other => vec![other],
    })
}

pub(crate) enum EvalScope<'a> {
    Root {
        row: &'a serde_json::Value,
    },
    Bound {
        row: &'a serde_json::Value,
        binding: &'a BindingName,
    },
}

impl<'a> EvalScope<'a> {
    fn row(&self) -> &'a serde_json::Value {
        match self {
            Self::Root { row } | Self::Bound { row, .. } => row,
        }
    }
}

pub(crate) struct InputEnv<'a> {
    pub(crate) rows: &'a BTreeMap<InputAlias, MaterializedInputRow>,
}

pub(crate) struct WireCoercionCtx<'a> {
    cgs: &'a CGS,
    source_entity: &'a plasm_core::EntityDef,
}

pub(crate) struct PlanEvalEnv<'a> {
    pub(crate) scope: EvalScope<'a>,
    pub(crate) inputs: InputEnv<'a>,
    pub(crate) wire_coercion: Option<WireCoercionCtx<'a>>,
}

pub(crate) fn eval_plan_value(
    value: &PlanValue,
    env: &PlanEvalEnv<'_>,
) -> Result<serde_json::Value, String> {
    match value {
        PlanValue::Literal { value } => Ok(value.clone()),
        PlanValue::Helper { display, args, .. } => Ok(display
            .as_ref()
            .map(|s| serde_json::Value::String(s.clone()))
            .unwrap_or_else(|| serde_json::Value::Array(args.clone()))),
        PlanValue::Symbol { path } => {
            let path = match &env.scope {
                EvalScope::Root { .. } => path.as_str(),
                EvalScope::Bound { binding, .. } => strip_binding(path, binding),
            };
            Ok(value_at_dotted(env.scope.row(), path)
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        }
        PlanValue::BindingSymbol { binding, path } => {
            let EvalScope::Bound {
                binding: scope_binding,
                ..
            } = &env.scope
            else {
                return Err(format!(
                    "binding symbol {binding:?} cannot resolve at root scope"
                ));
            };
            if scope_binding.as_str() != binding.as_str() {
                return Err(format!(
                    "binding symbol references unknown binding {binding:?}"
                ));
            }
            Ok(value_at_segments(env.scope.row(), path)
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        }
        PlanValue::NodeSymbol { node, alias, path } => {
            let alias = InputAlias::new(alias.clone())?;
            let expected_node = PlanNodeId::new(node.clone())?;
            let input = env.inputs.rows.get(&alias).ok_or_else(|| {
                format!(
                    "node symbol references missing input alias {:?}",
                    alias.as_str()
                )
            })?;
            if input.node != expected_node {
                return Err(format!(
                    "node symbol alias {:?} is bound to {:?}, not {:?}",
                    alias.as_str(),
                    input.node.as_str(),
                    expected_node.as_str()
                ));
            }
            match input.proof {
                crate::plasm_plan::InputCardinalityProof::StaticSingleton
                | crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton => {}
            }
            Ok(value_at_segments(&input.row, path)
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        }
        PlanValue::Template { template, .. } => {
            Ok(serde_json::Value::String(render_template(template, env)?))
        }
        PlanValue::EntityRefKey { key, .. } => eval_plan_value(key, env),
        PlanValue::Array { items } => Ok(serde_json::Value::Array(
            items
                .iter()
                .map(|item| eval_plan_value(item, env))
                .collect::<Result<Vec<_>, _>>()?,
        )),
        PlanValue::Object { fields } => {
            let mut out = serde_json::Map::new();
            for (k, v) in fields {
                out.insert(k.clone(), eval_plan_value(v, env)?);
            }
            Ok(serde_json::Value::Object(out))
        }
    }
}

pub(crate) fn strip_binding<'a>(path: &'a str, binding: &BindingName) -> &'a str {
    let binding = binding.as_str();
    if path == binding {
        return "";
    }
    if let Some(rest) = path.strip_prefix(&format!("{binding}.")) {
        return rest;
    }
    path
}

pub(crate) fn render_template(template: &str, env: &PlanEvalEnv<'_>) -> Result<String, String> {
    render_template_with(template, env, json_scalar_display)
}

#[cfg(test)]
pub(crate) fn render_expr_template(
    template: &str,
    env: &PlanEvalEnv<'_>,
) -> Result<String, String> {
    render_template_with(template, env, json_plasm_literal_display)
}

pub(crate) fn render_template_with(
    template: &str,
    env: &PlanEvalEnv<'_>,
    render_value: fn(&serde_json::Value) -> String,
) -> Result<String, String> {
    plasm_core::text::interpolate_dollar_template(
        template,
        |raw_path| {
            let rendered = resolve_template_path(raw_path, env)
                .map(render_value)
                .ok_or_else(|| format!("template path {raw_path:?} did not resolve"))?;
            Ok(rendered)
        },
        plasm_core::text::DEFAULT_MAX_INTERPOLATED_LEN,
    )
    .map(|t| t.into_string())
    .map_err(|e| e.to_string())
}

pub(crate) fn resolve_template_path<'a>(
    raw_path: &str,
    env: &'a PlanEvalEnv<'_>,
) -> Option<&'a serde_json::Value> {
    if let EvalScope::Bound { binding, .. } = &env.scope {
        if raw_path == binding.as_str() || raw_path.starts_with(&format!("{binding}.")) {
            return value_at_dotted(env.scope.row(), strip_binding(raw_path, binding));
        }
    }
    let (alias, rest) = raw_path
        .split_once('.')
        .map_or((raw_path, ""), |(alias, rest)| (alias, rest));
    let alias = InputAlias::new(alias.to_string()).ok()?;
    env.inputs
        .rows
        .get(&alias)
        .and_then(|input| value_at_dotted(&input.row, rest))
}

pub(crate) fn value_at_path<'a>(
    row: &'a serde_json::Value,
    path: &FieldPath,
) -> Option<&'a serde_json::Value> {
    let mut cur = row;
    for segment in path.segments() {
        cur = cur.get(segment)?;
    }
    Some(cur)
}

pub(crate) fn value_at_dotted<'a>(
    row: &'a serde_json::Value,
    path: &str,
) -> Option<&'a serde_json::Value> {
    if path.is_empty() {
        return Some(row);
    }
    let mut cur = row;
    for segment in path.split('.').filter(|s| !s.is_empty()) {
        cur = cur.get(segment)?;
    }
    Some(cur)
}
pub(crate) fn dry_validate_render_nodes(
    es: &ExecuteSession,
    plan: &crate::plasm_plan::Plan<crate::plasm_plan::ValidatedPlanState>,
) -> Result<(), String> {
    use crate::plasm_plan::{ComputeOp, ValidatedPlanNode};
    use std::collections::HashMap;

    let nodes: HashMap<String, &ValidatedPlanNode> = plan
        .nodes
        .iter()
        .map(|n| (n.id().as_str().to_string(), n))
        .collect();
    for n in &plan.nodes {
        let ValidatedPlanNode::Compute(c) = n else {
            continue;
        };
        let ComputeOp::Render {
            columns,
            template,
            column_aliases,
        } = &c.compute.op
        else {
            continue;
        };
        let qe = dry_render_source_qualified_entity(&nodes, c.compute.source.clone())?;
        let scoped = entry_scoped_execute_session(es, Some(&qe))?;
        let ent = scoped
            .cgs
            .get_entity(qe.entity.as_str())
            .ok_or_else(|| format!("dry render: unknown entity `{}`", qe.entity))?;
        let mut row = serde_json::Map::new();
        for field in ent.fields.keys() {
            row.insert(field.as_str().to_string(), serde_json::Value::Null);
        }
        row.insert(
            ent.id_field.as_str().to_string(),
            serde_json::Value::String("dry-placeholder".into()),
        );
        render_compute(
            &[serde_json::Value::Object(row)],
            &RenderColumns::from_op_parts(columns.clone(), column_aliases.clone()),
            template,
        )?;
    }
    Ok(())
}

fn dry_render_source_qualified_entity(
    nodes: &std::collections::HashMap<String, &ValidatedPlanNode>,
    mut source: String,
) -> Result<QualifiedEntityKey, String> {
    use crate::plasm_plan::ValidatedPlanNode;

    loop {
        let Some(n) = nodes.get(source.as_str()) else {
            return Err(format!("dry render: unknown source node `{source}`"));
        };
        match n {
            ValidatedPlanNode::Surface(s) => {
                return s.qualified_entity.clone().ok_or_else(|| {
                    format!("dry render: surface `{source}` has no qualified entity")
                });
            }
            ValidatedPlanNode::RelationTraversal(r) => return Ok(r.relation.target.clone()),
            ValidatedPlanNode::Compute(c) => {
                source = c.compute.source.clone();
            }
            other => {
                return Err(format!(
                    "dry render: source `{source}` is {:?}, expected surface/relation/compute chain",
                    other.kind()
                ));
            }
        }
    }
}

pub(crate) fn json_to_plasm_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(Value::Integer)
            .or_else(|| n.as_f64().map(Value::Float))
            .unwrap_or(Value::Null),
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            Value::Array(items.iter().map(json_to_plasm_value).collect())
        }
        serde_json::Value::Object(obj) => Value::Object(
            obj.iter()
                .map(|(k, v)| (k.clone(), json_to_plasm_value(v)))
                .collect::<IndexMap<_, _>>(),
        ),
    }
}
pub(crate) fn synthetic_projection(node: &ValidatedPlanNode) -> Option<Vec<String>> {
    match node {
        ValidatedPlanNode::Compute(compute) => Some(
            compute
                .compute
                .schema
                .fields
                .iter()
                .map(|f| f.name.as_str().to_string())
                .collect(),
        ),
        _ => None,
    }
}
