//! One binding boundary for live, hydration and preflight Get requests.
use super::*;

/// Sources expose bindings; resolution, precedence and identity projection live here.
pub(crate) trait GetBindings {
    fn bindings_for(&self, get: &GetExpr, cgs: &CGS) -> IndexMap<String, Value>;
}
impl GetBindings for ViewAmbientContext {
    fn bindings_for(&self, _: &GetExpr, _: &CGS) -> IndexMap<String, Value> {
        self.capability_params.clone()
    }
}
impl GetBindings for SessionMaterialization {
    fn bindings_for(&self, get: &GetExpr, cgs: &CGS) -> IndexMap<String, Value> {
        self.capability_params_for_get(
            &get.reference,
            &SessionMaterialization::provide_catalog_key(cgs, get.catalog_entry_id.as_deref()),
        )
    }
}
impl GetBindings for CapabilityParamEnv {
    fn bindings_for(&self, _: &GetExpr, _: &CGS) -> IndexMap<String, Value> {
        self.bindings().clone()
    }
}

#[derive(Clone, Copy)]
pub(crate) enum GetPurpose {
    Authored,
    Hydration,
}

/// Transport cannot accept an identity without its bound environment. Construction
/// pins the chosen capability and catalog, and uses the same projection in preflight.
pub(crate) struct ResolvedGet<'a> {
    pub(super) get: &'a GetExpr,
    pub(super) capability: &'a CapabilitySchema,
    pub(super) env: CmlEnv,
    pub(super) ambient: ViewAmbientContext,
}
impl<'a> ResolvedGet<'a> {
    pub(super) fn resolve(
        get: &'a GetExpr,
        cgs: &'a CGS,
        capability_name: Option<&str>,
        primary: &impl GetBindings,
        ambient: &ViewAmbientContext,
        purpose: GetPurpose,
    ) -> Result<Self, RuntimeError> {
        let name = capability_name.or(get.capability_name.as_deref());
        let capability = match name {
            Some(name) => cgs.get_capability(name),
            None => cgs.find_capability(&get.reference.entity_type, CapabilityKind::Get),
        }
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: name.unwrap_or("get").into(),
            entity: get.reference.entity_type.to_string(),
        })?;
        if capability.kind != CapabilityKind::Get || capability.domain != get.reference.entity_type
        {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "capability '{}' must be kind get for entity {}",
                    capability.name, get.reference.entity_type
                ),
            });
        }
        let mut bindings = primary.bindings_for(get, cgs);
        for (key, value) in &ambient.capability_params {
            bindings.entry(key.clone()).or_insert_with(|| value.clone());
        }
        let mut ambient = ambient.clone();
        ambient.capability_params = bindings.clone();
        let target = cgs
            .get_entity(get.reference.entity_type.as_str())
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!("unknown Get entity {}", get.reference.entity_type),
            })?;
        let mut env = CmlEnv::new();
        populate_template_path_env(
            &mut env,
            capability,
            &get.reference,
            plasm_core::IdentityProjectionCtx::Entity(target),
            Some(&Value::Object(bindings)),
        )?;
        normalize_cml_env_scope_entity_refs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability).map_err(|error| {
            RuntimeError::ConfigurationError {
                message: error.to_string(),
            }
        })?;
        if matches!(purpose, GetPurpose::Authored) {
            merge_plasm_execute_session_env(&mut env);
        }
        Ok(Self {
            get,
            capability,
            env,
            ambient,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]
        #[test]
        fn binding_adapters_preserve_parameters_and_typed_identity(
            id in 1i64..100000, token in "[a-zA-Z0-9:_/-]{1,40}",
        ) {
            let cgs = plasm_core::load_schema_dir(&std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/hydration_boundary_matrix")).unwrap();
            let get = GetExpr::from_ref(Ref::new("Note", id.to_string()));
            let bindings: IndexMap<String, Value> = IndexMap::from([
                ("access_token".into(), Value::String(token.clone())),
                ("id".into(), Value::Integer(-999)),
                ("note_id".into(), Value::String("foreign-identity".into())),
            ]);
            let bindings = serde_json::from_slice(&serde_json::to_vec(&bindings).unwrap()).unwrap();
            let ambient = ViewAmbientContext::default().with_capability_params(bindings);
            let cap = cgs.find_capability("Note", CapabilityKind::Get).unwrap();
            let inherited = CapabilityParamEnv::from_bindings(&ambient.capability_params, cap);
            let mut mat = SessionMaterialization::new();
            mat.stamp_capability_params(&get.reference, ambient.capability_params.clone());
            fn check(source: &impl GetBindings, get: &GetExpr, cgs: &CGS, token: &str, id: i64) {
                let wrong = ViewAmbientContext::default().with_capability_params(IndexMap::from([
                    ("access_token".into(), Value::String("must-not-overwrite".into()))]));
                let request = ResolvedGet::resolve(get,cgs,None,source,&wrong,GetPurpose::Hydration).unwrap();
                assert_eq!(request.env.get("access_token"), Some(&Value::String(token.into())));
                assert_eq!(request.env.get("note_id"), Some(&Value::String(id.to_string())));
            }
            check(&ambient,&get,&cgs,&token,id);
            check(&inherited,&get,&cgs,&token,id);
            check(&mat,&get,&cgs,&token,id);
            prop_assert!(ResolvedGet::resolve(&get,&cgs,Some("owner_get"),&ambient,&ambient,GetPurpose::Hydration).is_err());
            let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
            let template = compiled.capability(cap.name.as_str()).unwrap();
            let ready = ResolvedGet::resolve(&get,&cgs,None,&ambient,&ambient,GetPurpose::Hydration).unwrap();
            prop_assert!(compile_operation_dispatch(template, &ready.env).is_ok());
            let empty = ViewAmbientContext::default();
            let missing = ResolvedGet::resolve(&get,&cgs,None,&empty,&empty,GetPurpose::Hydration).unwrap();
            prop_assert!(compile_operation_dispatch(template, &missing.env).is_err());

        }
    }
}
