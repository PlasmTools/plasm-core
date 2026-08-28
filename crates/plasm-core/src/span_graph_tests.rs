//! Force-flush span-graph contracts for plasm-core hot-path spans.

#![cfg(test)]

use std::path::PathBuf;

use plasm_otel::span_capture::{find_span, is_descendant, with_captured_spans};

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join("fixtures/schemas")
}

#[test]
fn schema_load_path_parents_assemble_and_parse_program_exists() {
    let dir = fixtures_root().join("overshow_tools");
    assert!(dir.is_dir(), "expected fixture dir at {}", dir.display());

    let ((), spans) = with_captured_spans(|| {
        let cgs = crate::loader::load_schema(&dir).expect("load schema");
        // Even on parse failure the parse.program span must still be entered.
        let _ = crate::expr_parser::parse("not a valid program {{{", &cgs);
    });

    let load = find_span(&spans, "plasm_core.schema.load_path").expect("load_path span");
    let assemble = find_span(&spans, "plasm_core.schema.assemble").expect("assemble span");
    assert!(
        is_descendant(assemble, load, &spans),
        "assemble must be under load_path (got {:?})",
        spans.iter().map(|s| s.name.as_ref()).collect::<Vec<_>>()
    );
    assert!(
        find_span(&spans, "plasm_core.parse.program").is_some(),
        "parse.program span missing; got {:?}",
        spans.iter().map(|s| s.name.as_ref()).collect::<Vec<_>>()
    );
}
