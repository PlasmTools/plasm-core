//! CML template path / scope env population.

use super::*;

pub(crate) fn path_var_names_from_template(template: &CapabilityTemplate) -> Vec<String> {
    match template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            path_var_names_from_request(cml)
        }
        CapabilityTemplate::View(_) => Vec::new(),
        CapabilityTemplate::EvmCall(_) | CapabilityTemplate::EvmLogs(_) => Vec::new(),
    }
}

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

/// Bind template variables for get/delete/invoke:
/// explicit `path_vars` first, then keys from `input_overlay`, then identity slots
/// ([`ResolvedIdentity`]), while preserving the legacy HTTP single-path-var => `id` alias.
pub(crate) fn populate_template_path_env(
    env: &mut CmlEnv,
    template: &CapabilityTemplate,
    reference: &Ref,
    ent: Option<&plasm_core::schema::EntityDef>,
    path_vars: Option<&indexmap::IndexMap<String, Value>>,
    input_overlay: Option<&Value>,
) {
    let identity = plasm_core::ResolvedIdentity::from_ref(reference, ent);
    let primary_id = reference.primary_slot_str();
    let id_val = Value::String(primary_id.clone());

    for (k, v) in &identity.slots {
        env.insert(k.clone(), Value::String(v.clone()));
    }

    let single_http_id_alias = match template {
        CapabilityTemplate::Http(cml) | CapabilityTemplate::GraphQl(cml) => {
            let names = path_var_names_from_request(cml);
            (names.len() == 1).then(|| names[0].clone())
        }
        CapabilityTemplate::View(_)
        | CapabilityTemplate::EvmCall(_)
        | CapabilityTemplate::EvmLogs(_) => None,
    };

    for var_name in template_var_names(template) {
        if var_name == "id" {
            continue;
        }

        let resolved = path_vars
            .and_then(|m| m.get(&var_name))
            .cloned()
            .or_else(|| {
                input_overlay.and_then(|inp| match inp {
                    Value::Object(map) => map.get(&var_name).cloned(),
                    _ => None,
                })
            })
            .or_else(|| {
                identity
                    .get(&var_name)
                    .map(|s| Value::String(s.to_string()))
            })
            .or_else(|| {
                single_http_id_alias
                    .as_ref()
                    .filter(|name| *name == &var_name)
                    .map(|_| id_val.clone())
            });

        if let Some(value) = resolved {
            env.insert(var_name.clone(), value);
        }
    }

    // Explicit `path_vars` override compound [`Ref`] strings and template defaults (program
    // `node_input`, materialized entity-ref rows, …).
    if let Some(pv) = path_vars {
        for (k, v) in pv {
            env.insert(k.clone(), v.clone());
        }
    }
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
