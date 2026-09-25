//! Reproducible abstract-fixture card, independent of production catalogs.
use plasm_core::prompt_render::python::{prepare_python_teaching_wave, PythonTeachingState};
use plasm_core::TeachingExposureSession;

fn main() -> Result<(), String> {
    let cgs = plasm_core::loader::load_schema_dir(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/python_dag_slice"),
    )?;
    let mut exposure = TeachingExposureSession::new(&cgs, "fixture", &["Item"]);
    let first = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default())?;
    exposure.expose_entities(
        &[&cgs],
        std::sync::Arc::new(cgs.clone()),
        "fixture",
        &["Tag"],
    );
    let previous = match std::env::args().nth(1).as_deref() {
        Some("extension") => first.next_state,
        None | Some("full") => PythonTeachingState::default(),
        Some(_) => return Err("usage: python_teaching_card [full|extension]".into()),
    };
    let wave = prepare_python_teaching_wave(&exposure, &previous)?;
    print!(
        "# Generated from python_dag_slice; interface reference, not program source.\n{}",
        wave.declarations
    );
    Ok(())
}
