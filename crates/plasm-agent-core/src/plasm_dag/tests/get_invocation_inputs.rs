//! Get inputs use ordinary invocation contracts and producer dependencies.
use super::*;

fn schema() -> CGS {
    plasm_core::load_schema_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/schemas/hydration_boundary_matrix")).unwrap()
}

fn session() -> ExecuteSession { with_schema(schema()) }

fn with_schema(cgs: CGS) -> ExecuteSession {
    let cgs = Arc::new(cgs);
    let entry = "reads";
    let exposure = TeachingExposureSession::new(&cgs, entry, &["Note", "Owner", "Folder"]);
    ExecuteSession::new("ph".into(), "p".into(), cgs.clone(),
        indexmap::IndexMap::from([(entry.into(), Arc::new(CgsContext::entry(entry, cgs.clone())))]),
        entry.into(), String::new(), String::new(), None,
        vec!["Note".into(), "Owner".into(), "Folder".into()], Some(exposure), None,
        cgs.catalog_cgs_hash_hex(), None)
}

#[tokio::test]
async fn get_invocation_inputs_preserve_dependencies_and_codec() {
    let session = session();
    let symbols = session.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let note = symbols.entity_sym_for("reads", "Note");
    let owner = symbols.entity_sym_for("reads", "Owner");
    let source = format!("class Reads(Program):\n    def build(self):\n        owner = {owner}.get(identity=1)\n        return {note}.get(identity=7, access_token=owner.name)");
    let bundle = crate::plasm_compile::compile_python_program(&session, &source).await.unwrap();
    let wire = serde_json::to_value(&bundle.artifact().comp).unwrap();
    let restored = crate::plasm_comp_wire::plasm_comp_artifact_from_comp(serde_json::from_value(wire).unwrap()).unwrap();
    let restored = crate::plasm_comp_bundle::PlasmCompBundle::new(restored).unwrap();
    assert_eq!(bundle.artifact().comp, restored.artifact().comp);
    let wire = serde_json::to_string(&bundle.artifact().comp).unwrap();
    assert!(wire.contains("access_token"));
    assert!(bundle.artifact().comp.bind.topo.len() >= 2);
    assert!(bundle.artifact().comp.bind.holes.values().flatten().any(|hole| hole.step.as_str() == "owner"));
}

#[tokio::test]
async fn get_invocation_inputs_reject_missing_unknown_duplicate_and_wrong_type() {
    let session = session();
    let symbols = session.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let note = symbols.entity_sym_for("reads", "Note");
    for arguments in ["identity=7", "identity=7, access_token=[]", "identity=7, access_token='scope', extra='bad'",
        "identity=7, access_token='scope', access_token='other'", "7, identity=7, access_token='scope'"] {
        let source = format!("class Reads(Program):\n    def build(self):\n        return {note}.get({arguments})");
        assert!(crate::plasm_compile::compile_python_program(&session, &source).await.is_err(), "accepted {arguments}");
    }
    let source = format!("class Reads(Program):\n    def build(self):\n        return {note}.get(identity=7, access_token='scope')");
    crate::plasm_compile::compile_python_program(&session, &source).await.unwrap();
}

#[tokio::test]
async fn get_invocation_inputs_support_nullary_compound_and_selected_reads() {
    for nullary in [true, false] {
        let mut cgs = schema();
        if nullary {
            cgs.capabilities.get_mut("note_get").unwrap().inputs.receiver = Some(plasm_core::CapabilityReceiver::None);
        } else {
            cgs.entities.get_mut("Note").unwrap().key_vars = vec!["note_id".into(), "title".into()];
        }
        let mut secondary = cgs.capabilities["note_get"].clone();
        secondary.name = "selected_note_read".into();
        cgs.capabilities.insert(secondary.name.clone(), secondary);
        let session = with_schema(cgs);
        let symbols = session.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let note = symbols.entity_sym_for("reads", "Note");
        let selected = symbols.method_sym_for("reads", "Note", "selected_note_read");
        let arguments = if nullary { "note_id=7, access_token='scope'" } else { "note_id=7, title='example', access_token='scope'" };
        let source = format!("class Reads(Program):\n    def build(self):\n        return {note}.{selected}({arguments})");
        let bundle = crate::plasm_compile::compile_python_program(&session, &source).await.unwrap();
        let wire = serde_json::to_string(&bundle.artifact().comp).unwrap();
        assert!(wire.contains("selected_note_read"));
    }
}

#[test]
fn get_invocation_inputs_reject_reserved_identity_collision_in_cgs() {
    let mut cgs = schema();
    let plasm_core::InputType::Object { fields, .. } = &mut cgs.capabilities.get_mut("note_get").unwrap().inputs.arguments.as_mut().unwrap().input_type else { unreachable!() };
    fields[0].name = "identity".into();
    let error = cgs.validate().unwrap_err();
    assert!(matches!(error, plasm_core::SchemaError::SchemaConstraint(plasm_core::error::SchemaConstraintError::GetInputIdentityCollision { .. })));
    let mut cgs = schema();
    let cap = cgs.capabilities.get_mut("note_get").unwrap();
    let mut scope = cap.inputs.scope.0[0].clone();
    scope.name = "identity".into();
    cap.inputs.scope.0.push(scope);
    assert!(matches!(cgs.validate().unwrap_err(), plasm_core::SchemaError::SchemaConstraint(plasm_core::error::SchemaConstraintError::GetInputIdentityCollision { .. })));
    for discriminator_collision in [true, false] {
        let mut cgs = plasm_core::load_schema_dir(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_union_matrix")).unwrap();
        let mut input = cgs.capabilities["record_write"].inputs.payload.clone().unwrap();
        let plasm_core::InputType::Union { variants } = &mut input.input_type else { unreachable!() };
        if discriminator_collision {
            variants[0].wire.field = "identity".into();
        } else {
            variants[0].fields[0].name = "identity".into();
        }
        cgs.capabilities.get_mut("record_get").unwrap().inputs.arguments = Some(input);
        assert!(matches!(cgs.validate().unwrap_err(), plasm_core::SchemaError::SchemaConstraint(plasm_core::error::SchemaConstraintError::GetInputIdentityCollision { .. })));
    }
}
