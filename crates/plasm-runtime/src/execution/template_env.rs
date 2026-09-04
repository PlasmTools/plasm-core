//! CML template path / scope env population.

use super::*;

pub(crate) fn ensure_http_operation(
    operation: &CompiledOperation,
    action: &str,
) -> Result<(), RuntimeError> {
    if matches!(
        operation,
        CompiledOperation::Http(_) | CompiledOperation::GraphQl(_)
    ) {
        return Ok(());
    }
    Err(RuntimeError::UnsupportedExecutionMode {
        mode: format!("{action} with non-HTTP transport (phase 1 supports EVM reads only)"),
    })
}

/// Bind CML env from one identity projection + optional overlay.
///
/// [`project_capability_identity_env`] materializes [`ResolvedIdentity`] once and projects
/// path/GQL identity-env vars; declared inputs come from `input_overlay`.
pub(crate) fn populate_template_path_env(
    env: &mut CmlEnv,
    cap: &CapabilitySchema,
    reference: &Ref,
    ctx: plasm_core::IdentityProjectionCtx<'_>,
    input_overlay: Option<&Value>,
) -> Result<(), RuntimeError> {
    let projected = plasm_core::project_capability_identity_env(cap, reference, ctx).map_err(
        |e| RuntimeError::ConfigurationError {
            message: e.to_string(),
        },
    )?;
    for (k, v) in &projected.identity.slots {
        env.insert(k.clone(), v.clone());
    }
    for (k, v) in projected.path_env.slots {
        env.insert(k, v);
    }

    if let Some(Value::Object(map)) = input_overlay {
        for (k, v) in map {
            env.insert(k.clone(), v.clone());
        }
    }
    Ok(())
}

/// Narrow scope slots typed as [`FieldType::EntityRef`] (row JSON → id string, etc.).
pub(crate) fn normalize_cml_env_scope_entity_refs(
    env: &mut CmlEnv,
    cgs: &CGS,
    capability: &CapabilitySchema,
) -> Result<(), RuntimeError> {
    for field in capability.scope_params() {
        let nv = field
            .named_value(cgs)
            .map_err(|e| RuntimeError::ConfigurationError {
                message: format!("capability `{}`: {e}", capability.name),
            })?;
        let FieldType::EntityRef { target, .. } = &nv.field_type else {
            continue;
        };
        let Some(ent) = cgs.get_entity(target.as_str()) else {
            continue;
        };
        let Some(slot) = env.get_mut(&field.name) else {
            continue;
        };
        if let Some(norm) = normalize_cml_scope_entity_ref_value(slot, ent) {
            *slot = norm;
        }
    }
    Ok(())
}

pub(crate) fn normalize_cml_scope_entity_ref_value(
    value: &Value,
    ent: &EntityDef,
) -> Option<Value> {
    let normalized = plasm_core::normalize_entity_ref_value_for_target(value, ent)?;
    if ent.key_vars.len() >= 2 {
        return Some(normalized);
    }
    match &normalized {
        Value::Object(map) => {
            let key = ent
                .key_vars
                .first()
                .map(|k| k.as_str())
                .unwrap_or_else(|| ent.id_field.as_str());
            map.get(key).and_then(|v| match v {
                Value::String(_) | Value::Integer(_) | Value::Float(_) | Value::Bool(_) => {
                    Some(v.clone())
                }
                _ => None,
            })
        }
        other => Some(other.clone()),
    }
}
