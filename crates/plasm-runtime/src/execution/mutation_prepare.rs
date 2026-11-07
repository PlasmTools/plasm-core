//! Shared mutation input and binding preparation for preflight and execution.

use super::*;

pub(super) fn create<'a>(
    create: &plasm_core::CreateExpr,
    cgs: &'a CGS,
) -> Result<(&'a plasm_core::CapabilitySchema, CmlEnv), RuntimeError> {
    let capability = cgs
        .get_capability(create.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: create.capability.to_string(),
            entity: create.entity.to_string(),
        })?;
    let payload = if let Some(schema) = &capability.inputs.payload {
        InvokeInputPayload::lift(&create.input.to_value(), &schema.input_type, cgs)
    } else {
        create.input.clone()
    };
    let input = match &capability.inputs.payload {
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
    if let Value::Object(map) = input {
        env.extend(map);
    }
    normalize_cml_env_inputs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
    Ok((capability, env))
}

pub(super) fn targeted<'a>(
    call: &impl plasm_core::expr::TargetedCall,
    cgs: &'a CGS,
    mat: &SessionMaterialization,
) -> Result<
    (
        &'a plasm_core::CapabilitySchema,
        &'a plasm_core::schema::EntityDef,
        CmlEnv,
    ),
    RuntimeError,
> {
    let capability = cgs
        .get_capability(call.capability().as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: call.capability().to_string(),
            entity: call.target().entity_type.to_string(),
        })?;
    let target = cgs
        .get_entity(call.target().entity_type.as_str())
        .ok_or_else(|| RuntimeError::EntityUnknown {
            entity: call.target().entity_type.to_string(),
        })?;
    let raw = match call.input() {
        None => Value::Object(indexmap::IndexMap::new()),
        Some(input) => {
            let payload = if let Some(schema) = &capability.inputs.payload {
                InvokeInputPayload::lift(&input.to_value(), &schema.input_type, cgs)
            } else {
                input.clone()
            };
            match &capability.inputs.payload {
                Some(schema) => plasm_core::normalize_structured_string_inputs(
                    payload.to_value(),
                    &schema.input_type,
                    cgs,
                ),
                None => payload.to_value(),
            }
        }
    };
    let input = plasm_core::prepare_targeted_capability_input(capability, call, raw, cgs);
    let input = (!(call.input().is_none() && input.as_object().is_some_and(|m| m.is_empty())))
        .then_some(input);
    let mut overlay = mat.capability_params_for(call.target());
    if let Some(Value::Object(fields)) = &input {
        overlay.extend(fields.clone());
    }
    let overlay = (!overlay.is_empty()).then_some(Value::Object(overlay));
    let mut env = CmlEnv::new();
    populate_template_path_env(
        &mut env,
        capability,
        call.target(),
        plasm_core::IdentityProjectionCtx::Entity(target),
        overlay.as_ref(),
    )?;
    if let Some(input) = input {
        env.insert("input".to_string(), input);
    }
    normalize_cml_env_inputs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
    Ok((capability, target, env))
}

pub(super) fn invoke<'a>(
    call: &InvokeExpr,
    cgs: &'a CGS,
    mat: &SessionMaterialization,
) -> Result<
    (
        &'a plasm_core::CapabilitySchema,
        &'a plasm_core::schema::EntityDef,
        CmlEnv,
    ),
    RuntimeError,
> {
    let (capability, target, mut env) = targeted(call, cgs, mat)?;
    merge_entity_id_from_into_input_env(&mut env, Some(target), capability);
    Ok((capability, target, env))
}
