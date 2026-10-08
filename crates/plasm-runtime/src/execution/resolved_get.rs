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
    fn observation(&self, cgs: &CGS, row_version: u64) -> crate::materialization::GetObservation {
        crate::materialization::GetObservation {
            capability: self.capability.name.clone(),
            catalog: SessionMaterialization::provide_catalog_key(
                cgs,
                self.get.catalog_entry_id.as_deref(),
            ),
            environment: self.env.clone(),
            parameters: self.ambient.capability_params.clone(),
            explicit_parameters: self.get.input.as_ref().map(|_| {
                self.capability
                    .input_parameter_names()
                    .map(str::to_owned)
                    .collect()
            }),
            row_version,
        }
    }

    pub(super) fn cached_row<'m>(
        &self,
        mat: &'m SessionMaterialization,
        cgs: &CGS,
    ) -> Option<&'m CachedEntity> {
        let row = mat.consult_complete_get(&self.get.reference)?;
        match mat.get_observations.get(&self.get.reference) {
            Some(observation) => {
                let mut expected = self.observation(cgs, row.version);
                expected.explicit_parameters = observation.explicit_parameters.clone();
                if observation != &expected {
                    return None;
                }
            }
            None if self.get.input.is_some() => return None,
            _ => {}
        }
        Some(row)
    }

    pub(super) fn record(
        &self,
        mat: &mut SessionMaterialization,
        cgs: &CGS,
        row: CachedEntity,
    ) -> Result<(), RuntimeError> {
        let reference = row.reference.clone();
        if self.get.input.is_some() {
            // An explicit scoped read publishes its own snapshot, without fields from another scope.
            mat.publish_fresh_row(row)?;
        } else {
            mat.insert(row)?;
        }
        let Some(row) = mat.graph.get(&reference) else {
            return Ok(());
        };
        mat.get_observations
            .insert(reference.clone(), self.observation(cgs, row.version));
        if self.get.input.is_some() {
            let parameters = mat
                .inherited_capability_params
                .entry(reference)
                .or_default();
            let declared: Vec<_> = self.capability.input_parameter_names().collect();
            parameters.retain(|name, _| !declared.contains(&name.as_str()));
            parameters.extend(self.ambient.capability_params.clone());
        }
        Ok(())
    }

    pub(super) fn resolve(
        get: &'a GetExpr,
        cgs: &'a CGS,
        capability_name: Option<&str>,
        primary: &impl GetBindings,
        ambient: &ViewAmbientContext,
        purpose: GetPurpose,
    ) -> Result<Self, RuntimeError> {
        let capability =
            get.resolve_capability(cgs, capability_name.or(get.capability_name.as_deref()))?;
        let mut bindings = primary.bindings_for(get, cgs);
        for (key, value) in &ambient.capability_params {
            bindings.entry(key.clone()).or_insert_with(|| value.clone());
        }
        if let Some(input) = &get.input {
            let entity = cgs
                .get_entity(get.reference.entity_type.as_str())
                .ok_or_else(|| RuntimeError::EntityUnknown {
                    entity: get.reference.entity_type.to_string(),
                })?;
            let input = plasm_core::effective_capability_input(
                capability,
                entity,
                &get.reference,
                input.to_value(),
                cgs,
            );
            plasm_core::validate_capability_invocation_input(capability, &input, cgs)?;
            bindings.clear();
            if let Value::Object(explicit) = input {
                bindings = explicit;
            }
        }
        let mut ambient = ambient.clone();
        ambient.capability_params = bindings.clone();
        let target = cgs
            .get_entity(get.reference.entity_type.as_str())
            .ok_or_else(|| RuntimeError::EntityUnknown {
                entity: get.reference.entity_type.to_string(),
            })?;
        let mut env = CmlEnv::new();
        populate_template_path_env(
            &mut env,
            capability,
            &get.reference,
            plasm_core::IdentityProjectionCtx::Entity(target),
            Some(&Value::Object(bindings)),
        )?;
        normalize_cml_env_inputs(&mut env, cgs, capability)?;
        plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
        ambient.capability_params = CapabilityParamEnv::from_cml_env(&env, capability)
            .bindings()
            .clone();
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

    #[test]
    fn captured_input_names_include_root_union_fields() {
        let cgs = plasm_core::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_union_matrix"),
        )
        .unwrap();
        let cap = cgs.get_capability("record_write").unwrap();
        let env = IndexMap::from([
            ("kind".into(), Value::String("text".into())),
            ("text".into(), Value::String("content".into())),
            ("note".into(), Value::String("preserved".into())),
            ("unrelated".into(), Value::String("outside".into())),
        ]);
        let captured = CapabilityParamEnv::from_cml_env(&env, cap);
        for name in ["kind", "text", "note"] {
            assert_eq!(captured.bindings().get(name), env.get(name));
        }
        assert!(!captured.bindings().contains_key("unrelated"));
    }

    #[test]
    fn nullary_get_captures_inputs_on_the_decoded_identity() {
        let mut cgs = plasm_core::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/hydration_boundary_matrix"),
        )
        .unwrap();
        cgs.capabilities
            .get_mut("note_get")
            .unwrap()
            .inputs
            .receiver = Some(plasm_core::CapabilityReceiver::None);
        let mut get = GetExpr::new("Note", "").with_capability("note_get");
        get.input = Some(
            Value::Object(IndexMap::from([
                ("note_id".into(), Value::Integer(7)),
                ("access_token".into(), Value::String("explicit".into())),
            ]))
            .into(),
        );
        let ambient = ViewAmbientContext::default();
        let mut mat = SessionMaterialization::new();
        let request =
            ResolvedGet::resolve(&get, &cgs, None, &mat, &ambient, GetPurpose::Authored).unwrap();
        let reference = Ref::new("Note", "7");
        let row = CachedEntity::from_decoded(
            reference.clone(),
            IndexMap::from([
                ("note_id".into(), Value::Integer(7)),
                ("title".into(), Value::String("title".into())),
            ]),
            IndexMap::new(),
            0,
            EntityCompleteness::Complete,
        );
        request.record(&mut mat, &cgs, row).unwrap();
        assert_eq!(
            mat.capability_params_for_get(
                &reference,
                &SessionMaterialization::provide_catalog_key(&cgs, None)
            )["access_token"],
            Value::String("explicit".into())
        );
        assert!(!mat.get_observations.contains_key(&get.reference));
    }

    #[test]
    fn explicit_get_inputs_defaults_cache_scope_and_hydration_share_one_boundary() {
        let mut cgs = plasm_core::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/hydration_boundary_matrix"),
        )
        .unwrap();
        let cap = cgs.capabilities.get_mut("note_get").unwrap();
        cap.inputs.scope.0[0].default = Some(Value::Integer(99));
        let plasm_core::InputType::Object { fields, .. } =
            &mut cap.inputs.arguments.as_mut().unwrap().input_type
        else {
            unreachable!()
        };
        let mut region = fields[0].clone();
        region.name = "region".into();
        region.required = false;
        region.default = Some(Value::String("world".into()));
        fields.push(region);
        let mut optional = fields[0].clone();
        optional.name = "optional_hint".into();
        optional.required = false;
        fields.push(optional);
        let mut alternate = cap.clone();
        alternate.name = "alternate_get".into();
        cgs.capabilities.insert(alternate.name.clone(), alternate);
        let mut get = GetExpr::new("Note", "7").with_capability("note_get");
        get.input = Some(
            Value::Object(IndexMap::from([(
                "access_token".into(),
                Value::String("explicit".into()),
            )]))
            .into(),
        );
        let ambient = ViewAmbientContext::default().with_capability_params(IndexMap::from([
            ("access_token".into(), Value::String("old".into())),
            ("region".into(), Value::String("old-region".into())),
        ]));
        let mut mat = SessionMaterialization::new();
        let request =
            ResolvedGet::resolve(&get, &cgs, None, &mat, &ambient, GetPurpose::Authored).unwrap();
        assert_eq!(request.env["region"], Value::String("world".into()));
        match request.env.get("note_id") {
            Some(Value::Integer(id)) => assert_eq!(*id, 7),
            Some(Value::String(id)) => assert_eq!(id, "7"),
            value => panic!("receiver identity was replaced by a default: {value:?}"),
        }
        assert_eq!(
            request.env["access_token"],
            Value::String("explicit".into())
        );
        let row = CachedEntity::from_decoded(
            get.reference.clone(),
            IndexMap::from([
                ("note_id".into(), Value::Integer(7)),
                ("title".into(), Value::String("title".into())),
            ]),
            IndexMap::new(),
            0,
            EntityCompleteness::Complete,
        );
        request.record(&mut mat, &cgs, row.clone()).unwrap();
        assert!(request.cached_row(&mat, &cgs).is_some());
        let mut other = get.clone();
        other.capability_name = Some("alternate_get".into());
        let other =
            ResolvedGet::resolve(&other, &cgs, None, &mat, &ambient, GetPurpose::Authored).unwrap();
        assert!(other.cached_row(&mat, &cgs).is_none());
        let mut internal = get.clone();
        internal.input = None;
        internal.capability_name = Some("alternate_get".into());
        let internal =
            ResolvedGet::resolve(&internal, &cgs, None, &mat, &ambient, GetPurpose::Authored)
                .unwrap();
        assert!(internal.cached_row(&mat, &cgs).is_none());
        let mut scope = get.clone();
        scope.input = Some(
            Value::Object(IndexMap::from([(
                "access_token".into(),
                Value::String("other".into()),
            )]))
            .into(),
        );
        let scope =
            ResolvedGet::resolve(&scope, &cgs, None, &mat, &ambient, GetPurpose::Authored).unwrap();
        assert!(scope.cached_row(&mat, &cgs).is_none());
        let hydration = GetExpr::from_ref(get.reference.clone());
        let hydration = ResolvedGet::resolve(
            &hydration,
            &cgs,
            None,
            &mat,
            &ViewAmbientContext::default(),
            GetPurpose::Hydration,
        )
        .unwrap();
        assert_eq!(
            hydration.env["access_token"],
            Value::String("explicit".into())
        );
        mat.stamp_capability_params(
            &get.reference,
            IndexMap::from([
                ("optional_hint".into(), Value::String("old".into())),
                ("other_read_input".into(), Value::String("retained".into())),
            ]),
        );
        mat.stamp_provided_session_params(
            SessionMaterialization::provide_catalog_key(&cgs, None),
            IndexMap::from([("optional_hint".into(), Value::String("old".into()))]),
        );
        let mut branch =
            SessionMaterialization::seed_read_branch(&mat, mat.graph.fork_for_branch());
        request.record(&mut branch, &cgs, row.clone()).unwrap();
        mat.absorb_branch(branch).unwrap();
        assert_eq!(
            mat.capability_params_for(&get.reference)
                .get("other_read_input"),
            Some(&Value::String("retained".into()))
        );
        assert!(internal.cached_row(&mat, &cgs).is_none());
        let inherited = GetExpr::from_ref(get.reference.clone());
        let inherited = ResolvedGet::resolve(
            &inherited,
            &cgs,
            None,
            &mat,
            &ViewAmbientContext::default(),
            GetPurpose::Hydration,
        )
        .unwrap();
        assert!(
            !inherited.env.contains_key("optional_hint"),
            "branch absorption restored an omitted input"
        );
        let mut branch =
            SessionMaterialization::seed_read_branch(&mat, mat.graph.fork_for_branch());
        branch.publish_fresh_row(row).unwrap();
        mat.absorb_branch(branch).unwrap();
        assert!(
            request.cached_row(&mat, &cgs).is_none(),
            "a non-Get snapshot revived an old Get witness"
        );
        mat.poison_read_caches_after_mutation();
        assert!(request.cached_row(&mat, &cgs).is_none());
    }

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
