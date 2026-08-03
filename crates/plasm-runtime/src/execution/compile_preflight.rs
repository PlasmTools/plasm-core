//! CML compile gates without HTTP I/O — shared by dry preflight and live execute.

use super::*;
use crate::preflight::apply_preflight_compile_stubs;
use crate::view_plan::ViewAmbientContext;
use crate::view_preflight::{preflight_view_get, preflight_view_query};

/// Compile capability templates for `expr` without dispatching HTTP.
pub fn preflight_compile_expr(
    expr: &Expr,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
) -> Result<(), RuntimeError> {
    match expr {
        Expr::Query(query) => preflight_compile_query(query, cgs, ambient),
        Expr::Get(get) => preflight_compile_get(get, cgs, ambient),
        Expr::Create(create) => preflight_compile_create(create, cgs),
        Expr::Delete(delete) => preflight_compile_delete(delete, cgs),
        Expr::Invoke(invoke) => preflight_compile_invoke(invoke, cgs),
        Expr::Chain(chain) => preflight_compile_expr(&chain.source, cgs, ambient),
        Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) | Expr::TeachingValue { .. } => Ok(()),
    }
}

fn preflight_compile_query(
    query: &QueryExpr,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
) -> Result<(), RuntimeError> {
    let filter = compile_query_dispatch(query, cgs)?;
    let capability = resolve_query_capability(query, cgs)?;
    let mut env = CmlEnv::new();
    if let Some(f) = &filter {
        let json_val = f.to_json();
        env.insert("filter".to_string(), json_to_plasm_value(&json_val));
    }
    if let Some(pred) = &query.predicate {
        extract_predicate_vars(pred, &mut env);
    }
    normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
        RuntimeError::ConfigurationError {
            message: e.to_string(),
        }
    })?;
    if let Some(proj) = &query.projection {
        env.insert(
            "projection".to_string(),
            Value::Array(proj.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }
    let capability_template = parse_capability_template(&capability.mapping.template)?;
    if let CapabilityTemplate::View(vt) = &capability_template {
        return preflight_view_query(vt.view.as_str(), query, cgs, ambient);
    }
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

fn preflight_compile_get(
    get: &GetExpr,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
) -> Result<(), RuntimeError> {
    let capability = cgs
        .find_capability(&get.reference.entity_type, CapabilityKind::Get)
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: "get".to_string(),
            entity: get.reference.entity_type.to_string(),
        })?;
    let capability_template = parse_capability_template(&capability.mapping.template)?;
    if let CapabilityTemplate::View(vt) = &capability_template {
        return preflight_view_get(vt.view.as_str(), get, cgs, ambient);
    }
    let mut env = CmlEnv::new();
    merge_plasm_execute_session_share_token_env(&mut env);
    merge_plasm_execute_session_proof_base_token_env(&mut env);
    let target_ent = cgs.get_entity(get.reference.entity_type.as_str());
    populate_template_path_env(
        &mut env,
        &capability_template,
        &get.reference,
        target_ent,
        get.path_vars.as_ref(),
        None,
    );
    normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
        RuntimeError::ConfigurationError {
            message: e.to_string(),
        }
    })?;
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

fn preflight_compile_create(
    create: &plasm_core::CreateExpr,
    cgs: &CGS,
) -> Result<(), RuntimeError> {
    let capability = cgs
        .get_capability(create.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: create.capability.to_string(),
            entity: create.entity.to_string(),
        })?;
    let capability_template = parse_capability_template(&capability.mapping.template)?;
    let payload = if let Some(schema) = &capability.input_schema {
        InvokeInputPayload::lift(&create.input.to_value(), &schema.input_type, cgs)
    } else {
        create.input.clone()
    };
    let input = match capability.input_schema.as_ref() {
        Some(schema) => plasm_core::normalize_structured_string_inputs(
            payload.to_value(),
            &schema.input_type,
            cgs,
        ),
        None => payload.to_value(),
    };
    let input = plasm_core::prepare_create_capability_input(capability, create, input, cgs);
    let mut env = CmlEnv::new();
    merge_plasm_execute_session_share_token_env(&mut env);
    merge_plasm_execute_session_proof_base_token_env(&mut env);
    env.insert("input".to_string(), input.clone());
    if let Value::Object(ref map) = input {
        for var_name in path_var_names_from_template(&capability_template) {
            if let Some(v) = map.get(&var_name) {
                env.insert(var_name.clone(), v.clone());
            }
        }
        for (k, v) in map {
            env.insert(k.clone(), v.clone());
        }
    }
    normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
        RuntimeError::ConfigurationError {
            message: e.to_string(),
        }
    })?;
    apply_preflight_compile_stubs(&mut env, capability, cgs)?;
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

fn preflight_compile_delete(
    delete: &plasm_core::DeleteExpr,
    cgs: &CGS,
) -> Result<(), RuntimeError> {
    let capability = cgs
        .get_capability(delete.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: delete.capability.to_string(),
            entity: delete.target.entity_type.to_string(),
        })?;
    let capability_template = parse_capability_template(&capability.mapping.template)?;
    let mut env = CmlEnv::new();
    merge_plasm_execute_session_share_token_env(&mut env);
    merge_plasm_execute_session_proof_base_token_env(&mut env);
    let target_ent = cgs.get_entity(delete.target.entity_type.as_str());
    populate_template_path_env(
        &mut env,
        &capability_template,
        &delete.target,
        target_ent,
        delete.path_vars.as_ref(),
        None,
    );
    normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
        RuntimeError::ConfigurationError {
            message: e.to_string(),
        }
    })?;
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

fn preflight_compile_invoke(invoke: &InvokeExpr, cgs: &CGS) -> Result<(), RuntimeError> {
    let capability = cgs
        .get_capability(invoke.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: invoke.capability.to_string(),
            entity: invoke.target.entity_type.to_string(),
        })?;
    let capability_template = parse_capability_template(&capability.mapping.template)?;
    let input_for_env = {
        let raw = match &invoke.input {
            None => Value::Object(indexmap::IndexMap::new()),
            Some(input) => {
                let payload = if let Some(schema) = &capability.input_schema {
                    InvokeInputPayload::lift(&input.to_value(), &schema.input_type, cgs)
                } else {
                    input.clone()
                };
                match capability.input_schema.as_ref() {
                    Some(schema) => plasm_core::normalize_structured_string_inputs(
                        payload.to_value(),
                        &schema.input_type,
                        cgs,
                    ),
                    None => payload.to_value(),
                }
            }
        };
        let effective = plasm_core::prepare_invoke_capability_input(capability, invoke, raw, cgs);
        if invoke.input.is_none() && effective.as_object().is_some_and(|m| m.is_empty()) {
            None
        } else {
            Some(effective)
        }
    };
    let mut env = CmlEnv::new();
    merge_plasm_execute_session_share_token_env(&mut env);
    merge_plasm_execute_session_proof_base_token_env(&mut env);
    let target_ent = cgs.get_entity(invoke.target.entity_type.as_str());
    populate_template_path_env(
        &mut env,
        &capability_template,
        &invoke.target,
        target_ent,
        invoke.path_vars.as_ref(),
        input_for_env.as_ref(),
    );
    if let Some(input) = &input_for_env {
        env.insert("input".to_string(), input.clone());
        if let Value::Object(map) = input {
            for (k, v) in map {
                env.insert(k.clone(), v.clone());
            }
        }
    }
    normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|e| {
        RuntimeError::ConfigurationError {
            message: e.to_string(),
        }
    })?;
    merge_entity_id_from_into_input_env(&mut env, target_ent, capability);
    apply_preflight_compile_stubs(&mut env, capability, cgs)?;
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}
