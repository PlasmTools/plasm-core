//! CML compile gates without HTTP I/O — shared by dry preflight and live execute.

use super::*;
use crate::preflight::apply_preflight_compile_stubs;
use crate::view_plan::ViewAmbientContext;
use crate::view_plan::{derive_view_get_scope, derive_view_query_scope};
use crate::view_preflight::preflight_view_scoped_with_proof;

struct PreparedViewPreflight {
    name: String,
    scope: indexmap::IndexMap<String, Value>,
    ambient: ViewAmbientContext,
}

/// Compile capability templates for `expr` without dispatching HTTP.
///
/// Identity GETs inherit non-identity capability params from `mat`.
/// Canonical target identity is sealed after input binding. Dry-run and live share this gate.
pub fn preflight_compile_expr(
    expr: &Expr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<(), RuntimeError> {
    if let Some(view) = preflight_compile_leaf(expr, cgs, compiled, ambient, mat)? {
        preflight_view_scoped_with_proof(
            &view.name,
            view.scope,
            cgs,
            compiled,
            &view.ambient,
            mat,
        )?;
    }
    Ok(())
}

/// A view node compiles one transport operation; it cannot invoke the DAG driver.
/// Nested compositions are catalog faults, as in static view validation.
pub(crate) fn preflight_compile_view_node(
    expr: &Expr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
    view: &str,
    node: &str,
) -> Result<(), RuntimeError> {
    if preflight_compile_leaf(expr, cgs, compiled, ambient, mat)?.is_some() {
        return Err(plasm_compile::CatalogTemplateError::NestedView {
            view: view.to_owned(),
            node: node.to_owned(),
        }
        .into());
    }
    Ok(())
}

fn preflight_compile_leaf(
    mut expr: &Expr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<Option<PreparedViewPreflight>, RuntimeError> {
    loop {
        match expr {
            Expr::Chain(chain) => expr = &chain.source,
            Expr::Query(query) => return preflight_compile_query(query, cgs, compiled, ambient),
            Expr::Get(get) => return preflight_compile_get(get, cgs, compiled, ambient, mat),
            Expr::Create(create) => {
                return preflight_compile_create(create, cgs, compiled).map(|_| None)
            }
            Expr::Delete(delete) => {
                return preflight_compile_delete(delete, cgs, compiled).map(|_| None)
            }
            Expr::Invoke(invoke) => {
                return preflight_compile_invoke(invoke, cgs, compiled).map(|_| None)
            }
            Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) | Expr::TeachingValue { .. } => {
                return Ok(None)
            }
        }
    }
}

fn preflight_compile_query(
    query: &QueryExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
) -> Result<Option<PreparedViewPreflight>, RuntimeError> {
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
    normalize_cml_env_inputs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
    if let Some(proj) = &query.projection {
        env.insert(
            "projection".to_string(),
            Value::Array(proj.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
    if let CapabilityTemplate::View(vt) = &capability_template {
        return Ok(Some(PreparedViewPreflight {
            name: vt.view.to_string(),
            scope: derive_view_query_scope(vt.view.as_str(), query, cgs)?,
            ambient: ambient.clone(),
        }));
    }
    compile_operation_dispatch(&capability_template, &env).map(|_| None)
}

fn preflight_compile_get(
    get: &GetExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<Option<PreparedViewPreflight>, RuntimeError> {
    let request = ResolvedGet::resolve(
        get,
        cgs,
        get.capability_name.as_deref(),
        mat,
        ambient,
        GetPurpose::Authored,
    )?;
    let capability = request.capability;
    // List-backed derived Gets have no CML mapping; schema load already validated the plan.
    if capability.derived.is_some() {
        return Ok(None);
    }
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
    if let CapabilityTemplate::View(vt) = &capability_template {
        return Ok(Some(PreparedViewPreflight {
            name: vt.view.to_string(),
            scope: derive_view_get_scope(vt.view.as_str(), get, cgs)?,
            ambient: request.ambient,
        }));
    }
    compile_operation_dispatch(&capability_template, &request.env).map(|_| None)
}

fn preflight_compile_create(
    create: &plasm_core::CreateExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
) -> Result<(), RuntimeError> {
    let capability = cgs
        .get_capability(create.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: create.capability.to_string(),
            entity: create.entity.to_string(),
        })?;
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
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
    env.insert("input".to_string(), input.clone());
    if let Value::Object(ref map) = input {
        for (k, v) in map {
            env.insert(k.clone(), v.clone());
        }
    }
    normalize_cml_env_inputs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
    apply_preflight_compile_stubs(&mut env, capability, cgs);
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

fn preflight_compile_delete(
    delete: &plasm_core::DeleteExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
) -> Result<(), RuntimeError> {
    let capability = cgs
        .get_capability(delete.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: delete.capability.to_string(),
            entity: delete.target.entity_type.to_string(),
        })?;
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
    let input_for_env = targeted_call_input(delete, capability, cgs);
    let mut env = CmlEnv::new();
    let target_ent = cgs
        .get_entity(delete.target.entity_type.as_str())
        .ok_or_else(|| RuntimeError::EntityUnknown {
            entity: delete.target.entity_type.to_string(),
        })?;
    populate_template_path_env(
        &mut env,
        capability,
        &delete.target,
        plasm_core::IdentityProjectionCtx::Entity(target_ent),
        input_for_env.as_ref(),
    )?;
    if let Some(input) = input_for_env {
        env.insert("input".to_string(), input);
    }
    normalize_cml_env_inputs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

fn preflight_compile_invoke(
    invoke: &InvokeExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
) -> Result<(), RuntimeError> {
    let capability = cgs
        .get_capability(invoke.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: invoke.capability.to_string(),
            entity: invoke.target.entity_type.to_string(),
        })?;
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
    let input_for_env = targeted_call_input(invoke, capability, cgs);
    let mut env = CmlEnv::new();
    let target_ent = cgs
        .get_entity(invoke.target.entity_type.as_str())
        .ok_or_else(|| RuntimeError::EntityUnknown {
            entity: invoke.target.entity_type.to_string(),
        })?;
    populate_template_path_env(
        &mut env,
        capability,
        &invoke.target,
        plasm_core::IdentityProjectionCtx::Entity(target_ent),
        input_for_env.as_ref(),
    )?;
    if let Some(input) = &input_for_env {
        env.insert("input".to_string(), input.clone());
    }
    normalize_cml_env_inputs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
    merge_entity_id_from_into_input_env(&mut env, Some(target_ent), capability);
    apply_preflight_compile_stubs(&mut env, capability, cgs);
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

pub(super) fn targeted_call_input(
    invoke: &impl plasm_core::expr::TargetedCall,
    capability: &plasm_core::CapabilitySchema,
    cgs: &CGS,
) -> Option<Value> {
    let raw = match invoke.input() {
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
    let effective = plasm_core::prepare_targeted_capability_input(capability, invoke, raw, cgs);
    if invoke.input().is_none() && effective.as_object().is_some_and(|m| m.is_empty()) {
        None
    } else {
        Some(effective)
    }
}
