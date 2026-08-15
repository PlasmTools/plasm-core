use super::super::*;
use super::super::{value_at_dotted, value_at_segments};
use super::input_rows::materialized_result_use_inputs;
use std::collections::BTreeMap;

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
    let input_rows = materialized_result_use_inputs(materialized, uses_result, None)?;
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
            if !path.is_empty() && input.rows.len() > 1 {
                let mut values = Vec::with_capacity(input.rows.len());
                for (row, ident) in input.rows.iter().zip(input.row_identities.iter()) {
                    let cell = value_at_segments(row, &path)
                        .cloned()
                        .or_else(|| {
                            node_input_hole_from_identity(
                                env.wire_coercion.as_ref(),
                                ident,
                                &path,
                                row,
                            )
                        })
                        .unwrap_or(serde_json::Value::Null);
                    if !cell.is_null() {
                        values.push(coerce_node_input_json(
                            env.wire_coercion.as_ref(),
                            &path,
                            cell,
                        ));
                    }
                }
                return Ok(serde_json::Value::Array(values));
            }
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

/// Pure `derive` (map) row production: evaluate `value` once per source row under the item binding
/// scope plus singleton `inputs`. **PEC:** this is the *single* derive kernel — both live execute
/// ([`materialize_executable_plan_step`]) and dry preflight materialize derive rows through here, so
/// the only planned/live difference is the source of `source_rows` (I/O), never the derivation.
pub(crate) fn derive_node_rows(
    item_binding: &BindingName,
    value: &PlanValue,
    source_rows: &[serde_json::Value],
    input_rows: &BTreeMap<InputAlias, MaterializedInputRow>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = Vec::with_capacity(source_rows.len());
    for row in source_rows {
        let scope = EvalScope::Bound {
            row,
            binding: item_binding,
        };
        let inputs = InputEnv { rows: input_rows };
        let env = PlanEvalEnv {
            scope,
            inputs,
            wire_coercion: None,
        };
        rows.push(eval_plan_value(value, &env)?);
    }
    Ok(rows)
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

pub(crate) use plasm_core::json_value_to_plasm_value as json_to_plasm_value;

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
