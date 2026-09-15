//! RA-12: `| select` on a Query/Get/Search binding admits the taught entity field
//! set, not sheared read `provides`. Fixture: `return_projection_teaching`.

use super::*;
use crate::compile_plasm_program;
use indexmap::IndexMap;
use plasm_core::{CgsContext, PromptPipelineConfig, TeachingExposureSession};
use std::path::PathBuf;
use std::sync::Arc;

fn return_projection_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        plasm_core::loader::load_schema_dir(
            &root.join("../../fixtures/schemas/return_projection_teaching"),
        )
        .expect("load return_projection_teaching"),
    );
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        "return_projection_teaching".into(),
        Arc::new(CgsContext::entry("return_projection_teaching", cgs.clone())),
    );
    let exp = TeachingExposureSession::new(
        cgs.as_ref(),
        "return_projection_teaching",
        &["Notice", "Transaction"],
    );
    ExecuteSession::new(
        "ph".into(),
        String::new(),
        cgs.clone(),
        ctxs,
        "return_projection_teaching".into(),
        String::new(),
        String::new(),
        None,
        vec!["Notice".into(), "Transaction".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

fn compile_rp(name: &str, source: &str) -> Result<crate::PlasmCompBundle, String> {
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &return_projection_session(),
        name,
        source,
    )
    .map_err(|e| e.to_string())
}

#[test]
fn ra12_select_admits_taught_fields_outside_read_provides() {
    compile_rp(
        "ra12_select_notice_body",
        r#"rows = Notice
projected = rows | select body, author_email
projected"#,
    )
    .expect("Notice query teaches body/author_email; sheared provides must not reject | select");

    compile_rp(
        "ra12_select_transaction_private",
        r#"txns = Transaction
projected = txns | select created_at, private
projected"#,
    )
    .expect(
        "Transaction query teaches created_at/private; sheared provides must not reject | select",
    );
}

#[test]
fn ra12_select_still_rejects_unknown_wire() {
    let err = compile_rp(
        "ra12_select_unknown",
        r#"rows = Notice
bad = rows | select not_a_taught_field
bad"#,
    )
    .expect_err("unknown wires remain rejected");
    assert!(
        err.contains("not a row field"),
        "expected row-field diagnostic, got {err}"
    );
}
