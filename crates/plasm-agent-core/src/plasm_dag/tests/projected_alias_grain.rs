//! RA-2: `| select dest = src` grain is the projected columns, not the source entity.
//!
//! T184015 c77 submitted `sent | select email = receiver_email` then
//! `union (recv | select email)` against Venmo Transaction (no `email` row field).
//! This fixture is the abstract analog. On current HEAD those programs compile —
//! the “projected alias rejected as a source-entity field” hypothesis is false here.
//! Keep the tests so that class of diagnostic cannot be re-misdiagnosed as a compiler hole.
//!
//! Fixture: `projected_alias_grain` (Transfer has receiver_email/sender_email, not email).

use super::*;
use crate::compile_plasm_program;
use indexmap::IndexMap;
use plasm_core::{CgsContext, PromptPipelineConfig, TeachingExposureSession};
use std::path::PathBuf;
use std::sync::Arc;

fn projected_alias_session() -> ExecuteSession {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cgs = Arc::new(
        plasm_core::loader::load_schema_dir(
            &root.join("../../fixtures/schemas/projected_alias_grain"),
        )
        .expect("load projected_alias_grain"),
    );
    let mut ctxs = IndexMap::new();
    ctxs.insert(
        "projected_alias_grain".into(),
        Arc::new(CgsContext::entry("projected_alias_grain", cgs.clone())),
    );
    let exp = TeachingExposureSession::new(
        cgs.as_ref(),
        "projected_alias_grain",
        &["Transfer", "Contact"],
    );
    ExecuteSession::new(
        "ph".into(),
        String::new(),
        cgs.clone(),
        ctxs,
        "projected_alias_grain".into(),
        String::new(),
        String::new(),
        None,
        vec!["Transfer".into(), "Contact".into()],
        Some(exp),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    )
}

fn compile_grain(name: &str, source: &str) -> Result<crate::PlasmCompBundle, String> {
    compile_plasm_program(
        &PromptPipelineConfig::default(),
        None,
        &projected_alias_session(),
        name,
        source,
    )
    .map_err(|e| e.to_string())
}

/// c77 shape: rename, re-project the alias, union, membership on a real `email` entity.
const C77_SHAPE: &str = r#"sent = Transfer
received = Transfer
sent_peers = sent | select email = receiver_email
recv_peers = received | select email = sender_email
peers = sent_peers | union (recv_peers | select email) | distinct
to_keep = Contact | where email not in (peers | select email)
sent_peers, recv_peers, peers, to_keep"#;

#[test]
fn select_alias_rename_is_projected_grain() {
    compile_grain(
        "rename_only",
        r#"sent = Transfer
sent_peers = sent | select email = receiver_email
sent_peers"#,
    )
    .expect(
        "| select email = receiver_email must type dest as projected grain, not Transfer.email",
    );
}

#[test]
fn select_alias_reproject_after_rename() {
    compile_grain(
        "reproject_alias",
        r#"sent = Transfer
sent_peers = sent | select email = receiver_email
again = sent_peers | select email
again"#,
    )
    .expect("subsequent | select email must consult the projected grain, not Transfer");
}

#[test]
fn select_alias_union_and_membership_c77_shape() {
    compile_grain("c77_shape", C77_SHAPE).expect(
        "union of renamed columns plus (peers | select email) must keep the projected alias",
    );
}

#[test]
fn select_alias_e_sym_reproject_and_union() {
    compile_grain(
        "e_sym_c77_shape",
        r#"sent = e1
received = e1
sent_peers = sent | select email = receiver_email
recv_peers = received | select email = sender_email
peers = sent_peers | union (recv_peers | select email) | distinct
to_keep = e2 | where email not in (peers | select email)
sent_peers, recv_peers, peers, to_keep"#,
    )
    .expect("e# program of the c77 select/union/membership shape is lawful on this fixture");
}

#[test]
fn select_unknown_wire_still_rejected() {
    let err = compile_grain(
        "unknown_wire",
        r#"rows = Transfer
bad = rows | select not_a_taught_field
bad"#,
    )
    .expect_err("unknown wires remain rejected");
    assert!(
        err.contains("not a row field"),
        "expected row-field diagnostic, got {err}"
    );
}
