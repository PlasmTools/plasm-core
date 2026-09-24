//! CML template path / scope env population.

use super::*;

pub(crate) fn ensure_mutating_operation(
    operation: &CompiledOperation,
    action: &str,
) -> Result<(), RuntimeError> {
    if matches!(
        operation,
        CompiledOperation::Http(_)
            | CompiledOperation::GraphQl(_)
            | CompiledOperation::CredentialBind(_)
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
    let projected =
        plasm_core::project_capability_identity_env(cap, reference, ctx).map_err(|e| {
            RuntimeError::ConfigurationError {
                message: e.to_string(),
            }
        })?;
    let mut inputs = env.clone();
    if let Some(Value::Object(map)) = input_overlay {
        inputs.extend(map.clone());
    }
    *env = projected.bind(inputs);
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

#[cfg(test)]
mod identity_binding_laws {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        #[test]
        fn serialized_identity_projects_unchanged_into_every_targeted_transport(
            path in "/[a-zA-Z0-9_ /.-]{1,64}", foreign in any::<i64>(),
        ) {
            let cgs = plasm_core::load_schema_dir(&std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/target_identity_matrix")).unwrap();
            let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
            let reference = Ref::new("Document", path.clone());
            let reference: Ref = serde_json::from_slice(&serde_json::to_vec(&reference).unwrap()).unwrap();
            for name in ["document_get", "document_touch", "document_delete"] {
                let cap = cgs.get_capability(name).unwrap();
                let inputs = Value::Object(IndexMap::from([
                    ("id".into(), Value::Integer(foreign)),
                    ("path".into(), Value::String("wrong".into())),
                    ("unrelated".into(), Value::Integer(foreign)),
                ]));
                let mut env = CmlEnv::new();
                populate_template_path_env(&mut env, cap, &reference,
                    plasm_core::IdentityProjectionCtx::Entity(cgs.get_entity("Document").unwrap()),
                    Some(&inputs)).unwrap();
                prop_assert_eq!(env.get("unrelated"), Some(&Value::Integer(foreign)));
                let operation = compile_operation_dispatch(compiled.capability(name).unwrap(), &env).unwrap();
                let CompiledOperation::Http(request) = operation else { panic!("expected HTTP") };
                let Some(Value::Object(slots)) = request.query.or(request.body) else { panic!("missing target") };
                prop_assert_eq!(&slots["path"], &Value::String(path.clone()));
            }
        }
    }
}
