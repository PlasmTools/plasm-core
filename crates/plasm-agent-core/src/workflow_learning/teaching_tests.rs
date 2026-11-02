use super::*;
use std::sync::Arc;

pub(crate) fn session(entities: &[&str]) -> ExecuteSession {
    let root = std::path::PathBuf::from(crate::test_manifest_dir());
    let cgs = Arc::new(
        plasm_core::loader::load_schema_dir(
            &root.join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("matrix"),
    );
    session_with_schema(entities, cgs)
}

fn session_with_schema(entities: &[&str], cgs: Arc<plasm_core::CGS>) -> ExecuteSession {
    let mut session = crate::test_support::session_fixtures::ExecuteSessionFixture::new()
        .entry_id("langmatrix")
        .entities(entities.iter().map(|s| s.to_string()).collect())
        .build(cgs.clone());
    session.prompt_hash = crate::execute_path_ids::PromptHashHex::from_prompt_sha256(
        "workflow-learning matrix fixture",
    )
    .to_string();
    session.teaching_exposure = Some(plasm_core::TeachingExposureSession::new(
        &cgs,
        "langmatrix",
        entities,
    ));
    session
}

pub(crate) fn source(session: &ExecuteSession) -> String {
    let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
        session,
        cross_cache: None,
    });
    let entity = symbols.entity_sym_for("langmatrix", "LangItem");
    format!(
        "class Example(Program):\n    def build(self):\n        return {entity}.query().take(1)\n"
    )
}

#[test]
fn portable_rebinds_permuted_entity_exposure_and_rechecks() {
    let first = session(&["LangItem", "LangLine"]);
    let second = session(&["LangLine", "LangItem"]);
    let source = source(&first);
    let bundle =
        crate::plasm_dag::compile_python_program_checked(&first, &source).expect("compile");
    let portable = TeachingProgram::from_compiled(&first, &source, &bundle).expect("portable");
    assert!(portable
        .required_capabilities
        .iter()
        .any(|cap| cap.catalog == "langmatrix" && cap.capability == "langitem_query"));
    assert_eq!(portable.outputs.len(), 1);
    assert_eq!(
        portable.rebind(&second).expect("rebind"),
        super::tests::source(&second)
    );
    let rebound = portable.rebind(&second).unwrap();
    let bundle = crate::plasm_dag::compile_python_program_checked(&second, &rebound).unwrap();
    let other = TeachingProgram::from_compiled(&second, &rebound, &bundle).unwrap();
    assert_eq!(
        portable.structural_fingerprint,
        other.structural_fingerprint
    );
}

#[test]
fn nominal_value_annotations_relocate_without_rewriting_text_or_locals() {
    let first = session(&["LangItem", "LangLine"]);
    let second = session(&["LangLine", "LangItem"]);
    let domains = value_domains(&first).unwrap();
    let (old, reference) = domains
        .iter()
        .find(|(_, reference)| {
            matches!(reference,
        SemanticReference::ValueDomain { value_ref, .. } if value_ref == "nv_lang_item_id")
        })
        .expect("matrix item nominal domain");
    let new = value_domains(&second)
        .unwrap()
        .into_iter()
        .find(|(_, candidate)| candidate == reference)
        .unwrap()
        .0;
    assert_ne!(old, &new, "fixture must permute nominal types");
    let source = format!("class Example(Program):\n    @compute\n    def identity(self, row: Row[e1]) -> {old}:\n        return row.id\n    def build(self):\n        # {old} is text here\n        v999 = 'literal {old}'\n        item = e1.query().take(1)\n        return self.identity(item)\n");
    let compiled = crate::plasm_dag::compile_python_program_checked(&first, &source).unwrap();
    let teaching = TeachingProgram::from_compiled(&first, &source, &compiled).unwrap();
    let rebound = teaching.rebind_teaching(&second).unwrap();
    assert!(rebound.source.contains(&format!("-> {new}:")));
    assert!(rebound.source.contains(&format!("# {old} is text here")));
    assert!(rebound.source.contains(&format!("v999 = 'literal {old}'")));
    crate::plasm_dag::compile_python_program_checked(&second, &rebound.source).unwrap();
}

#[test]
fn persisted_source_tampering_is_rejected() {
    let session = session(&["LangItem"]);
    let source = source(&session);
    let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    let mut portable = TeachingProgram::from_compiled(&session, &source, &bundle).unwrap();
    portable.source = portable.source.replace("take(1)", "take(2)");
    assert!(matches!(
        portable.rebind(&session),
        Err(TeachingError::InvalidLedger)
    ));
}

#[test]
fn comments_are_neutral_but_bounds_are_not() {
    let session = session(&["LangItem"]);
    let source = source(&session);
    let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
        session: &session,
        cross_cache: None,
    });
    let a = TeachingProgram::extract(&source, symbols.as_ref(), &session).unwrap();
    let b =
        TeachingProgram::extract(&format!("# é e1\n{source}"), symbols.as_ref(), &session).unwrap();
    let c = TeachingProgram::extract(
        &source.replace("take(1)", "take(2)"),
        symbols.as_ref(),
        &session,
    )
    .unwrap();
    assert_eq!(a.structural_fingerprint, b.structural_fingerprint);
    assert_ne!(a.structural_fingerprint, c.structural_fingerprint);
}

#[test]
fn shadowed_symbols_and_recorded_state_are_not_portable() {
    let session = session(&["LangItem"]);
    let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
        session: &session,
        cross_cache: None,
    });
    for source in [
        "e1 = 2",
        "def f(e1):\n    return e1",
        "self.recorded('pr123')",
        "def e1():\n    pass",
    ] {
        assert!(matches!(
            TeachingProgram::extract(source, symbols.as_ref(), &session),
            Err(TeachingError::HiddenState)
        ));
    }
}

#[test]
fn tampered_catalog_pins_and_semantic_contracts_are_not_served() {
    let session = session(&["LangItem"]);
    let source = source(&session);
    let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    let mut portable = TeachingProgram::from_compiled(&session, &source, &bundle).unwrap();
    let original = portable.catalog_fingerprints.clone();
    portable
        .catalog_fingerprints
        .insert("langmatrix".into(), "changed".into());
    assert!(portable.rebind(&session).is_err());
    portable.catalog_fingerprints = original;
    portable
        .semantic_contracts
        .insert("changed-effect".into(), "changed".into());
    assert!(matches!(
        portable.rebind(&session),
        Err(TeachingError::InvalidLedger)
    ));
}

#[test]
fn compiler_version_drift_requires_revalidation() {
    let session = session(&["LangItem"]);
    let source = source(&session);
    let compiled = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    let mut portable = TeachingProgram::from_compiled(&session, &source, &compiled).unwrap();
    portable.compiler_version = "different-compiler".into();
    assert!(matches!(
        portable.rebind(&session),
        Err(TeachingError::NeedsRevalidation)
    ));
}

#[test]
fn wire_field_names_are_never_relocated() {
    let session = session(&["LangItem"]);
    let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
        session: &session,
        cross_cache: None,
    });
    let entity = symbols.entity_sym_for("langmatrix", "LangItem");
    let source = format!("rows = {entity}\nvalue = rows.p1\n");
    let portable = TeachingProgram::extract(&source, symbols.as_ref(), &session).unwrap();
    assert_eq!(portable.occurrences.len(), 1);
    assert!(portable
        .substitute(|_| Ok("e99".into()))
        .unwrap()
        .contains("rows.p1"));
}

#[test]
fn additive_catalog_revision_recompiles_but_effect_contract_drift_fails() {
    let original = session(&["LangItem"]);
    let source = source(&original);
    let bundle = crate::plasm_dag::compile_python_program_checked(&original, &source).unwrap();
    let portable = TeachingProgram::from_compiled(&original, &source, &bundle).unwrap();
    let mut current = original.clone();
    let mut cgs = (*current.cgs).clone();
    cgs.version += 1;
    let mut unrelated = cgs.get_entity("LangItem").unwrap().clone();
    unrelated.name = "Unrelated".into();
    cgs.entities.insert("Unrelated".into(), unrelated);
    current.cgs = Arc::new(cgs);
    current.contexts_by_entry.insert(
        "langmatrix".into(),
        Arc::new(plasm_core::CgsContext::entry(
            "langmatrix",
            current.cgs.clone(),
        )),
    );
    current.teaching_exposure = Some(plasm_core::TeachingExposureSession::new(
        &current.cgs,
        "langmatrix",
        &["LangItem"],
    ));
    let rebound = portable.rebind_teaching(&current).unwrap();
    assert_ne!(rebound.catalog_fingerprints, portable.catalog_fingerprints);
    assert_eq!(rebound.semantic_contracts, portable.semantic_contracts);
    let mut changed = (*current.cgs).clone();
    changed
        .capabilities
        .get_mut("langitem_query")
        .unwrap()
        .description
        .push_str(" different domain effect");
    current.cgs = Arc::new(changed);
    current.contexts_by_entry.insert(
        "langmatrix".into(),
        Arc::new(plasm_core::CgsContext::entry(
            "langmatrix",
            current.cgs.clone(),
        )),
    );
    current.teaching_exposure = Some(plasm_core::TeachingExposureSession::new(
        &current.cgs,
        "langmatrix",
        &["LangItem"],
    ));
    assert!(crate::plasm_dag::compile_python_program_checked(&current, &source).is_ok());
    assert!(portable.rebind(&current).is_err());
}

pub(crate) fn checked_fixture() -> CheckedTeachingExample {
    let session = session(&["LangItem"]);
    let source = source(&session);
    let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    CheckedTeachingExample {
        program: TeachingProgram::from_compiled(&session, &source, &bundle).unwrap(),
    }
}

pub(crate) fn checked_predicate_fixture() -> CheckedTeachingExample {
    let session = session(&["LangItem"]);
    let source = source(&session).replace(
        ".query()",
        ".query().where(lambda row: row.id == row.title)",
    );
    let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    CheckedTeachingExample {
        program: TeachingProgram::from_compiled(&session, &source, &bundle).unwrap(),
    }
}

#[test]
fn portability_errors_retain_parser_and_symbol_sources() {
    let session = session(&["LangItem"]);
    let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
        session: &session,
        cross_cache: None,
    });
    let parse = TeachingProgram::extract("class", symbols.as_ref(), &session).unwrap_err();
    assert!(matches!(parse, TeachingError::Parse(_)));
    assert!(std::error::Error::source(&parse)
        .unwrap()
        .is::<Box<ruff_python_parser::ParseError>>());
    let symbol = reference_for_token(symbols.as_ref(), "e999999").unwrap_err();
    assert!(matches!(symbol, TeachingError::SymbolResolution(_)));
    assert!(std::error::Error::source(&symbol)
        .unwrap()
        .is::<Box<plasm_core::symbol_tuning::SymbolResolveError>>());
}

#[test]
fn preservation_mismatch_reports_category_hashes_and_first_difference() {
    let session = session(&["LangItem"]);
    let original = source(&session);
    let changed = original.replace("take(1)", "take(2)");
    let original = crate::plasm_dag::compile_python_program_checked(&session, &original).unwrap();
    let changed = crate::plasm_dag::compile_python_program_checked(&session, &changed).unwrap();
    assert!(!compiled_preserves(&original, &changed));
    let TeachingError::PreservationMismatch {
        category,
        original_hash,
        recompiled_hash,
        path,
    } = preservation_error(&original, &changed)
    else {
        panic!("typed diagnostic")
    };
    assert_eq!(category, PreservationCategory::CanonicalComputation);
    assert_ne!(original_hash, recompiled_hash);
    assert_eq!(original_hash.len(), 64);
    assert!(!path.is_empty());
}

pub(crate) fn sensitive_fixture() -> (ExecuteSession, String, PlasmCompBundle) {
    let mut session = session(&["LangItem"]);
    let mut cgs = (*session.cgs).clone();
    let cap = cgs.capabilities.get_mut("langitem_create").unwrap();
    let plasm_core::InputType::Object { fields, .. } =
        &mut cap.inputs.payload.as_mut().unwrap().input_type
    else {
        panic!("matrix object")
    };
    fields.retain(|field| field.name == "active");
    for field in fields {
        field.required = false;
        if field.name == "active" {
            field.sink_class = Some(plasm_core::SinkClassName::new("credential").unwrap());
        }
    }
    cap.mapping.as_mut().unwrap().template.0["body"] = serde_json::json!({"type":"object", "fields":[["active", {"type":"var", "name":"active"}]]});
    session = session_with_schema(&["LangItem"], Arc::new(cgs));
    let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
        session: &session,
        cross_cache: None,
    });
    let entity = symbols.entity_sym_for("langmatrix", "LangItem");
    let method = symbols.method_sym_for("langmatrix", "LangItem", "langitem_create");
    let source = format!(
        "class Example(Program):\n    def build(self):\n        return {entity}.{method}(active=True)\n"
    );
    let compiled = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    (session, source, compiled)
}

#[test]
fn sensitive_boolean_catalog_input_is_withheld_but_retains_catalogue_evidence() {
    let (session, source, compiled) = sensitive_fixture();
    let observation = ProgramObservation::from_execution(&session, &source, &compiled);
    assert!(observation.available().unwrap().model_source().is_none());
    assert!(matches!(
        CheckedTeachingExample::from_observation(&observation),
        Err(TeachingError::SensitiveOperand { .. })
    ));
    assert!(
        !ProgramCatalogueRequirements::from_compiled(&session, &compiled)
            .unwrap()
            .required_capabilities()
            .is_empty()
    );
}

#[test]
fn classified_numeric_predicate_is_not_inferred_safe_from_comparison_syntax() {
    let mut session = session(&["LangItem"]);
    let mut cgs = (*session.cgs).clone();
    let class = plasm_core::schema::DataClassName::new("private_value").unwrap();
    cgs.data_classes.insert(
        class.clone(),
        plasm_core::schema::DataClassSchema {
            description: "Synthetic confidential scalar".into(),
            severity: plasm_core::schema::DataClassSeverity::Sensitive,
            dimension: None,
        },
    );
    cgs.entities
        .get_mut("LangItem")
        .unwrap()
        .fields
        .get_mut("score")
        .unwrap()
        .data_class = Some(class);
    session = session_with_schema(&["LangItem"], Arc::new(cgs));
    let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
        session: &session,
        cross_cache: None,
    });
    let entity = symbols.entity_sym_for("langmatrix", "LangItem");
    let source = format!(
        "class Example(Program):\n    def build(self):\n        return {entity}.query().where(lambda row: row.score == 123456)\n"
    );
    let compiled = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    let observation = ProgramObservation::from_execution(&session, &source, &compiled);
    assert!(observation.available().unwrap().model_source().is_none());
    assert!(matches!(
        CheckedTeachingExample::from_observation(&observation),
        Err(TeachingError::SensitiveOperand { .. })
    ));
    assert!(
        !ProgramCatalogueRequirements::from_compiled(&session, &compiled)
            .unwrap()
            .required_capabilities()
            .is_empty()
    );
    for (comparison, withheld) in [
        ("private_value == 123456", true),
        ("private_value == other_value", false),
    ] {
        let source = format!("class Example(Program):\n    def build(self):\n        item = {entity}.query().take(1)\n        private_value = item.score\n        other_value = item.score\n        result = {comparison}\n        return result\n");
        let compiled = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
        let observation = ProgramObservation::from_execution(&session, &source, &compiled);
        assert_eq!(
            observation.available().unwrap().model_source().is_none(),
            withheld,
            "{comparison}"
        );
    }
}

#[tokio::test]
async fn unannotated_literals_remain_exact_whole_program_teaching() {
    let original = session(&["LangItem", "LangLine"]);
    let reordered = session(&["LangLine", "LangItem"]);
    let source = "# exact authored example\nclass Example(Program):\n    def build(self):\n        first = e1.get(\"example-item\")\n        second = e1.query().where(lambda row: row.title == \"authored title\").take(2)\n        return first, second\n";
    let compiled =
        crate::plasm_compile::compile_program(&Default::default(), None, &original, "test", source)
            .await
            .unwrap();
    let observation = ProgramObservation::from_execution(&original, source, &compiled);
    let checked = CheckedTeachingExample::from_observation(&observation).unwrap();
    assert_eq!(checked.program().source, source);
    assert_eq!(checked.program().outputs.len(), 2);
    let rebound = checked.program().rebind_teaching(&reordered).unwrap();
    assert_eq!(rebound.source, source.replace("e1.", "e2."));
    crate::plasm_compile::compile_program(
        &Default::default(),
        None,
        &reordered,
        "test",
        &rebound.source,
    )
    .await
    .unwrap();
    let requirements = ProgramCatalogueRequirements::from_compiled(&original, &compiled).unwrap();
    assert!(requirements
        .required_capabilities()
        .iter()
        .any(|cap| cap.capability == "langitem_get"));
    assert!(requirements
        .required_capabilities()
        .iter()
        .any(|cap| cap.capability == "langitem_query"));
}

#[test]
fn durable_teaching_format_is_required_and_does_not_accept_template_payloads() {
    let checked = checked_fixture();
    let mut value = serde_json::to_value(&checked).unwrap();
    assert_eq!(value["program"]["format"], "whole_program_example");
    value["program"].as_object_mut().unwrap().remove("format");
    value["program"]["inputs"] = serde_json::json!([]);
    assert!(serde_json::from_value::<CheckedTeachingExample>(value).is_err());
}

#[test]
fn observation_restores_only_original_catalogues_after_unrelated_session_expansion() {
    let mut session = session(&["LangItem"]);
    let source = source(&session);
    let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    let expected = ProgramObservation::from_execution(&session, &source, &bundle);
    let requirements = ProgramCatalogueRequirements::from_compiled(&session, &bundle).unwrap();
    session.contexts_by_entry.insert(
        "unrelated".into(),
        Arc::new(plasm_core::CgsContext::entry(
            "unrelated",
            session.cgs.clone(),
        )),
    );
    assert_eq!(
        ProgramObservation::from_execution(&session, &source, &bundle),
        expected
    );
    assert_eq!(
        ProgramCatalogueRequirements::from_compiled(&session, &bundle).unwrap(),
        requirements
    );
    assert!(!requirements
        .catalog_fingerprints()
        .contains_key("unrelated"));
    session.contexts_by_entry.shift_remove("langmatrix");
    assert!(
        ProgramObservation::from_execution(&session, &source, &bundle)
            .available()
            .is_none()
    );
    assert!(ProgramCatalogueRequirements::from_compiled(&session, &bundle).is_err());
}

#[test]
fn direct_projection_uses_compilation_symbols_not_caller_numbering() {
    let original = session(&["LangItem", "LangLine"]);
    let reordered = session(&["LangLine", "LangItem"]);
    let source = source(&original);
    let bundle = crate::plasm_dag::compile_python_program_checked(&original, &source).unwrap();
    let expected = TeachingProgram::from_compiled(&original, &source, &bundle).unwrap();
    let observed = TeachingProgram::from_compiled(&reordered, &source, &bundle).unwrap();
    assert_eq!(observed, expected);
    assert_eq!(observed.dependencies[0].entity, "LangItem");
}

#[test]
fn sensitive_sink_distinguishes_literal_aliases_from_retrieved_dataflow() {
    let (session, source, _) = sensitive_fixture();
    for (bindings, value, embedded) in [
        ("", "True", true),
        ("        secret = True\n", "secret", true),
        (
            "        secret = True\n        alias = secret\n",
            "alias",
            true,
        ),
        (
            "        prior = e1.query().take(1)\n",
            "prior.active",
            false,
        ),
        (
            "        prior = e1.query().take(1)\n        secret = prior.active\n",
            "secret",
            false,
        ),
    ] {
        let authored = source
            .replace("        return", &format!("{bindings}        return"))
            .replace("active=True", &format!("active={value}"));
        let bundle = crate::plasm_dag::compile_python_program_checked(&session, &authored).unwrap();
        let observation = ProgramObservation::from_execution(&session, &authored, &bundle);
        let observed = observation.available().unwrap();
        if embedded {
            assert!(
                matches!(
                    observed.disclosure(),
                    DisclosureAssessment::KnownSensitiveOperands { .. }
                ),
                "{value}: {:?}",
                observed.disclosure()
            );
            assert!(matches!(
                CheckedTeachingExample::from_observation(&observation),
                Err(TeachingError::SensitiveOperand { .. })
            ));
            assert!(observed.model_source().is_none());
        } else {
            assert_eq!(
                observed.disclosure(),
                &DisclosureAssessment::NoKnownSensitiveOperands,
                "{value}"
            );
            assert_eq!(observed.model_source(), Some(authored.as_str()));
            assert!(CheckedTeachingExample::from_observation(&observation).is_ok());
        }
    }
}

#[test]
fn unsupported_sensitive_computation_is_uncertain_not_claimed_literal_disclosure() {
    let (session, source, _) = sensitive_fixture();
    let source = source
        .replace(
            "        return",
            "        secret = bool(True)\n        return",
        )
        .replace("active=True", "active=secret");
    let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
    let observation = ProgramObservation::from_execution(&session, &source, &bundle);
    assert!(matches!(
        observation.available().unwrap().disclosure(),
        DisclosureAssessment::UnresolvedSensitiveDataflow { .. }
    ));
    assert!(matches!(
        CheckedTeachingExample::from_observation(&observation),
        Err(TeachingError::UnresolvedSensitiveDataflow { .. })
    ));
}

#[test]
fn catalogue_semantic_context_is_literal_neutral_but_keeps_contract_differences() {
    let session = session(&["LangItem"]);
    let requirements = |session: &ExecuteSession, identity: &str| {
        let source = format!(
            "class Example(Program):\n    def build(self):\n        return e1.get(\"{identity}\")\n"
        );
        let bundle = crate::plasm_dag::compile_python_program_checked(session, &source).unwrap();
        ProgramCatalogueRequirements::from_compiled(session, &bundle).unwrap()
    };
    let first = requirements(&session, "item-a");
    let second = requirements(&session, "item-b");
    assert_eq!(first.capability_contracts(), second.capability_contracts());
    assert_ne!(first.source_sha256(), second.source_sha256());
    let contract = &first.capability_contracts()[0];
    assert_eq!(contract.kind, plasm_core::CapabilityKind::Get);
    // This Get has implicit receiver identity and no declared input fields.
    assert!(contract.inputs.scope.0.is_empty());
    assert!(contract.value_domains.is_empty());
    assert!(contract.provided_fields.contains_key("id"));
    assert!(!contract.provided_fields.is_empty());
    let mut cgs = (*session.cgs).clone();
    let cap = cgs.capabilities.get_mut("langitem_get").unwrap();
    cap.description = "Reads from the provider-owned restricted corpus".into();
    cap.inputs.input_validation.allow_null = true;
    let changed = session_with_schema(&["LangItem"], Arc::new(cgs));
    let changed = requirements(&changed, "item-a");
    assert_ne!(first.capability_contracts(), changed.capability_contracts());
    assert!(
        changed.capability_contracts()[0]
            .inputs
            .input_validation
            .allow_null
    );
    let serialized = serde_json::to_value(first.capability_contracts()).unwrap();
    assert!(serialized[0].get("mapping").is_none());
    assert!(serialized[0].get("auth").is_none());
    assert!(serialized[0].get("decoder").is_none());
}

#[test]
fn catalogue_context_excludes_authored_defaults_from_all_input_lanes() {
    let (session, _, _) = sensitive_fixture();
    let mut cgs = (*session.cgs).clone();
    let cap = cgs.capabilities.get_mut("langitem_create").unwrap();
    let plasm_core::InputType::Object { fields, .. } =
        &mut cap.inputs.payload.as_mut().unwrap().input_type
    else {
        panic!("payload")
    };
    fields[0].default = Some(plasm_core::Value::Bool(true));
    let reference = plasm_core::prerequisites::CapabilityRef {
        catalog: "langmatrix".into(),
        capability: "langitem_create".into(),
    };
    let projected = CapabilityTeachingContract::from_catalog(
        &reference,
        cgs.get_capability("langitem_create").unwrap(),
        &cgs,
    )
    .unwrap();
    let plasm_core::InputType::Object { fields, .. } = projected.inputs.payload.unwrap().input_type
    else {
        panic!("payload")
    };
    assert!(fields[0].default.is_none());
}

#[test]
fn complete_relation_programs_use_compiler_materialization_dependencies() {
    let session = session(&["LangItem", "LangLine", "LangTag", "CompoundBranch"]);
    let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
        session: &session,
        cross_cache: None,
    });
    let entity = symbols.entity_sym_for("langmatrix", "LangItem");
    for (relation, required) in [
        ("lines", "langitem_get"),
        ("tags", "langtag_query"),
        ("tags_by_score", "langtag_query"),
        ("compound_branches", "langcompoundbranch_get"),
    ] {
        let relation_symbol = symbols.ident_sym_relation_for("langmatrix", "LangItem", relation);
        let source = format!(
            "class Example(Program):\n    def build(self):\n        rows = {entity}.query().take(2)\n        return rows.flat_map(lambda item: item.{relation_symbol})\n"
        );
        let bundle = crate::plasm_dag::compile_python_program_checked(&session, &source).unwrap();
        let observation = ProgramObservation::from_execution(&session, &source, &bundle);
        let checked = CheckedTeachingExample::from_observation(&observation)
            .unwrap_or_else(|error| panic!("{relation}: {error}"));
        assert_eq!(checked.program().source, source);
        assert!(
            checked
                .program()
                .required_capabilities
                .iter()
                .any(|cap| cap.capability == required),
            "{relation}"
        );
        let requirements = ProgramCatalogueRequirements::from_compiled(&session, &bundle).unwrap();
        assert!(requirements
            .required_capabilities()
            .iter()
            .any(|cap| cap.capability == required));
    }
}

#[test]
fn explicit_nominal_domains_do_not_admit_entities_or_capabilities_and_survive_later_waves() {
    let mut session = session(&["LangItem"]);
    let mut exposure = session.teaching_exposure.take().unwrap();
    let before_entities = exposure.entities.clone();
    let before_caps = exposure.surface.capabilities.clone();
    exposure.expose_entities(&[session.cgs.as_ref()], session.cgs.clone(), "types", &[]);
    let initial = plasm_core::prompt_render::python::prepare_python_teaching_wave(
        &exposure,
        &session.python_teaching,
    )
    .unwrap();
    let wave = plasm_core::prompt_render::python::prepare_python_domain_wave(
        &exposure,
        &initial.next_state,
        &[(
            "types".into(),
            plasm_core::ValueDomainKey::new("nv_lang_item_score").unwrap(),
        )],
    )
    .unwrap();
    let (symbol, contract) = wave
        .value_contracts
        .iter()
        .find(|(_, contract)| {
            contract
                .domain
                .as_ref()
                .is_some_and(|d| d.entry_id == "types")
        })
        .unwrap();
    assert!(wave.declarations.contains(symbol));
    assert_eq!(exposure.entities, before_entities);
    assert_eq!(exposure.surface.capabilities, before_caps);
    let state = serde_json::from_value(serde_json::to_value(&wave.next_state).unwrap()).unwrap();
    exposure.expose_entities(
        &[session.cgs.as_ref()],
        session.cgs.clone(),
        "langmatrix",
        &["LangLine"],
    );
    let later =
        plasm_core::prompt_render::python::prepare_python_teaching_wave(&exposure, &state).unwrap();
    assert_eq!(&later.value_contracts[symbol], contract);
}

#[test]
fn compilation_restores_original_python_nominal_alias_state() {
    let mut original = session(&["LangItem", "LangLine"]);
    let wave = plasm_core::prompt_render::python::prepare_python_teaching_wave(
        original.teaching_exposure.as_ref().unwrap(),
        &original.python_teaching,
    )
    .unwrap();
    original.python_teaching = wave.next_state;
    let source = source(&original);
    let compiled = crate::plasm_dag::compile_python_program_checked(&original, &source).unwrap();
    let mut caller = session(&["LangLine", "LangItem"]);
    caller.python_teaching = plasm_core::prompt_render::python::prepare_python_teaching_wave(
        caller.teaching_exposure.as_ref().unwrap(),
        &caller.python_teaching,
    )
    .unwrap()
    .next_state;
    assert_ne!(caller.python_teaching, original.python_teaching);
    let restored = compiled
        .compilation_symbols()
        .captured()
        .unwrap()
        .restore_symbol_authority(&caller)
        .unwrap();
    assert_eq!(restored.python_teaching, original.python_teaching);
}

#[test]
fn annotation_only_catalogue_is_pinned_without_fabricated_capability() {
    let mut original = session(&["LangItem"]);
    let context = Arc::new(plasm_core::CgsContext::entry("types", original.cgs.clone()));
    original.contexts_by_entry.insert("types".into(), context);
    let exposure = original.teaching_exposure.as_mut().unwrap();
    exposure.expose_entities(&[original.cgs.as_ref()], original.cgs.clone(), "types", &[]);
    original.python_teaching = plasm_core::prompt_render::python::prepare_python_domain_wave(
        exposure,
        &original.python_teaching,
        &[(
            "types".into(),
            plasm_core::ValueDomainKey::new("nv_lang_item_score").unwrap(),
        )],
    )
    .unwrap()
    .next_state;
    let domains = value_domains(&original).unwrap();
    let token = domains
        .iter()
        .find_map(|(symbol, domain)| {
            matches!(domain, SemanticReference::ValueDomain { entry_id, .. } if entry_id == "types")
                .then_some(symbol)
        })
        .unwrap();
    let source = format!("class Example(Program):\n    @compute\n    def number(self, row: Row[e1]) -> {token}:\n        return 123\n    def build(self):\n        item = e1.query().take(1)\n        return self.number(item)\n");
    let compiled = crate::plasm_dag::compile_python_program_checked(&original, &source).unwrap();
    let teaching = TeachingProgram::from_compiled(&original, &source, &compiled).unwrap();
    assert!(teaching.catalog_fingerprints.contains_key("types"));
    assert!(teaching
        .semantic_contracts
        .contains_key("value:types:nv_lang_item_score"));
    assert!(!teaching
        .required_capabilities
        .iter()
        .any(|cap| cap.catalog == "types"));
    assert!(!teaching
        .dependencies
        .iter()
        .any(|dependency| dependency.entry_id == "types"));
    teaching.rebind_teaching(&original).unwrap();
}

#[test]
fn earlier_whole_program_witness_cannot_supply_nominal_symbol_authority() {
    for previous_version in [1, 2] {
        let mut checked = checked_fixture();
        checked.program.schema_version = previous_version;
        // The previous representation's AST fingerprint could remain internally valid;
        // version rejection must stand independently of that structural check.
        checked.program.structural_fingerprint = checked.program.fingerprint().unwrap();
        let decoded: CheckedTeachingExample =
            serde_json::from_value(serde_json::to_value(&checked).unwrap()).unwrap();
        assert!(!decoded.integrity_valid());
        assert!(matches!(
            decoded.program.rebind_teaching(&session(&["LangItem"])),
            Err(TeachingError::NeedsRevalidation)
        ));
    }
}

#[test]
fn catalogue_requirements_use_captured_deployment_authority_and_reject_missing_binding() {
    use plasm_core::prerequisites::{CapabilityRef, DeploymentBinding, DeploymentBindings};
    let root = std::path::PathBuf::from(crate::test_manifest_dir());
    let cgs = plasm_core::loader::load_schema_dir(
        &root.join("../../fixtures/schemas/prerequisite_matrix"),
    )
    .unwrap();
    let mut original = session_with_schema(&["BusinessRecord", "ProviderResult"], Arc::new(cgs));
    let symbols = resolve_session_symbol_map(&SessionSymbolMapContext {
        session: &original,
        cross_cache: None,
    });
    let method = symbols.method_sym_for("langmatrix", "ProviderResult", "acquire");
    let source = format!("class Example(Program):\n    def build(self):\n        access = e2.{method}(account='account-a')\n        return e1.query(credential=access.value)\n");
    let missing = crate::plasm_dag::compile_python_program_checked(&original, &source).unwrap();
    assert!(
        matches!(ProgramCatalogueRequirements::from_compiled(&original, &missing), Err(TeachingError::Prerequisite(error)) if matches!(*error, plasm_core::prerequisites::PrerequisiteError::MissingDeploymentBinding))
    );
    let bindings = DeploymentBindings {
        bindings: vec![DeploymentBinding {
            consumer: CapabilityRef {
                catalog: "langmatrix".into(),
                capability: "read".into(),
            },
            requirement: "scoped_access".into(),
            provider_catalog: "langmatrix".into(),
            provider: "value_source".into(),
        }],
    };
    original.set_prerequisite_deployments(bindings.clone());
    let compiled = crate::plasm_dag::compile_python_program_checked(&original, &source).unwrap();
    let expected = ProgramCatalogueRequirements::from_compiled(&original, &compiled).unwrap();
    assert!(expected
        .required_capabilities()
        .iter()
        .any(|cap| cap.capability == "acquire"));
    let serialized = serde_json::to_value(compiled.compilation_symbols()).unwrap();
    let decoded: crate::program_symbol_context::CompilationSymbolContext =
        serde_json::from_value(serialized.clone()).unwrap();
    original.set_prerequisite_deployments(Default::default());
    let restored = decoded
        .captured()
        .unwrap()
        .restore_symbol_authority(&original)
        .unwrap();
    assert_eq!(restored.prerequisite_deployments, bindings);
    assert_eq!(
        ProgramCatalogueRequirements::from_compiled(&original, &compiled).unwrap(),
        expected
    );
    let mut historical = serialized;
    historical["value"]
        .as_object_mut()
        .unwrap()
        .remove("prerequisite_deployments");
    let decoded: crate::program_symbol_context::CompilationSymbolContext =
        serde_json::from_value(historical.clone()).unwrap();
    assert_eq!(serde_json::to_value(&decoded).unwrap(), historical);
    assert!(matches!(
        decoded
            .captured()
            .unwrap()
            .restore_symbol_authority(&original),
        Err(crate::program_symbol_context::SymbolContextError::MissingDeploymentAuthority)
    ));
}
