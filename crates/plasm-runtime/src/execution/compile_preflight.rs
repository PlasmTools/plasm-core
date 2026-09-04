//! CML compile gates without HTTP I/O — shared by dry preflight and live execute.

use super::*;
use crate::preflight::apply_preflight_compile_stubs;
use crate::view_plan::ViewAmbientContext;
use crate::view_preflight::{preflight_view_get, preflight_view_query};

/// Compile capability templates for `expr` without dispatching HTTP.
///
/// Identity GETs inherit session-stamped capability params from `mat` (explicit
/// `path_vars` still win). Dry-run and live share this gate.
pub fn preflight_compile_expr(
    expr: &Expr,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<(), RuntimeError> {
    match expr {
        Expr::Query(query) => preflight_compile_query(query, cgs, ambient, mat),
        Expr::Get(get) => preflight_compile_get(get, cgs, ambient, mat),
        Expr::Create(create) => preflight_compile_create(create, cgs),
        Expr::Delete(delete) => preflight_compile_delete(delete, cgs),
        Expr::Invoke(invoke) => preflight_compile_invoke(invoke, cgs),
        Expr::Chain(chain) => preflight_compile_expr(&chain.source, cgs, ambient, mat),
        Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) | Expr::TeachingValue { .. } => Ok(()),
    }
}

fn preflight_compile_query(
    query: &QueryExpr,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
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
    let capability_template = parse_capability_template(
        &capability
            .require_mapping()
            .map_err(|message| RuntimeError::ConfigurationError { message })?
            .template,
    )?;
    if let CapabilityTemplate::View(vt) = &capability_template {
        return preflight_view_query(vt.view.as_str(), query, cgs, ambient, mat);
    }
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

fn resolve_get_capability_for_preflight<'a>(
    get: &GetExpr,
    cgs: &'a CGS,
) -> Result<&'a plasm_core::CapabilitySchema, RuntimeError> {
    match get.capability_name.as_deref() {
        Some(name) => {
            let c = cgs
                .get_capability(name)
                .ok_or_else(|| RuntimeError::CapabilityNotFound {
                    capability: name.to_string(),
                    entity: get.reference.entity_type.to_string(),
                })?;
            if c.kind != CapabilityKind::Get {
                return Err(RuntimeError::ConfigurationError {
                    message: format!("capability '{name}' must be kind get"),
                });
            }
            if c.domain.as_str() != get.reference.entity_type.as_str() {
                return Err(RuntimeError::ConfigurationError {
                    message: format!(
                        "capability '{name}' is for entity {}, expected {}",
                        c.domain.as_str(),
                        get.reference.entity_type
                    ),
                });
            }
            Ok(c)
        }
        None => cgs
            .find_capability(&get.reference.entity_type, CapabilityKind::Get)
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: "get".to_string(),
                entity: get.reference.entity_type.to_string(),
            }),
    }
}

fn preflight_compile_get(
    get: &GetExpr,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<(), RuntimeError> {
    let get = get_with_session_params(get, cgs, mat);
    let capability = resolve_get_capability_for_preflight(&get, cgs)?;
    // List-backed derived Gets have no CML mapping; schema load already validated the plan.
    if capability.derived.is_some() {
        return Ok(());
    }
    let mapping = capability.mapping.as_ref().ok_or_else(|| {
        RuntimeError::ConfigurationError {
            message: format!(
                "capability '{}' has neither CML mapping nor derived plan",
                capability.name
            ),
        }
    })?;
    let capability_template = parse_capability_template(&mapping.template)?;
    if let CapabilityTemplate::View(vt) = &capability_template {
        return preflight_view_get(vt.view.as_str(), &get, cgs, ambient, mat);
    }
    let mut env = CmlEnv::new();
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
    let capability_template = parse_capability_template(
        &capability
            .require_mapping()
            .map_err(|message| RuntimeError::ConfigurationError { message })?
            .template,
    )?;
    let payload = if let Some(schema) = &capability.inputs.payload {
        InvokeInputPayload::lift(&create.input.to_value(), &schema.input_type, cgs)
    } else {
        create.input.clone()
    };
    let input = match capability.inputs.payload.as_ref() {
        Some(schema) => plasm_core::normalize_structured_string_inputs(
            payload.to_value(),
            &schema.input_type,
            cgs,
        ),
        None => payload.to_value(),
    };
    let input = plasm_core::prepare_create_capability_input(capability, create, input, cgs);
    let mut env = CmlEnv::new();
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
    apply_preflight_compile_stubs(&mut env, capability, cgs);
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
    let capability_template = parse_capability_template(
        &capability
            .require_mapping()
            .map_err(|message| RuntimeError::ConfigurationError { message })?
            .template,
    )?;
    let mut env = CmlEnv::new();
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
    let capability_template = parse_capability_template(
        &capability
            .require_mapping()
            .map_err(|message| RuntimeError::ConfigurationError { message })?
            .template,
    )?;
    let input_for_env = {
        let raw = match &invoke.input {
            None => Value::Object(indexmap::IndexMap::new()),
            Some(input) => {
                let payload = if let Some(schema) = &capability.inputs.payload {
                    InvokeInputPayload::lift(&input.to_value(), &schema.input_type, cgs)
                } else {
                    input.clone()
                };
                match capability.inputs.payload.as_ref() {
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
    apply_preflight_compile_stubs(&mut env, capability, cgs);
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}
