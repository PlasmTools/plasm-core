//! Typed discovery against the checked-in `pokeapi_mini` split schema (lexical-only, no embeddings).

use std::path::PathBuf;
use std::sync::Arc;

use plasm_core::load_split_schema;
use plasm_discovery::{AgentDiscovery, DiscoveryDecision, DiscoveryQuery, TypedDiscovery};

fn pokeapi_fixture_paths() -> (PathBuf, PathBuf) {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = crate_dir.join("../../..");
    let domain = root.join("fixtures/schemas/pokeapi_mini/domain.yaml");
    let mappings = root.join("fixtures/schemas/pokeapi_mini/mappings.yaml");
    assert!(
        domain.is_file(),
        "missing fixture: {} (run from repo with fixtures/)",
        domain.display()
    );
    (domain, mappings)
}

#[tokio::test]
async fn lexical_ready_resolves_berry_query() {
    let (domain, mappings) = pokeapi_fixture_paths();
    let cgs = load_split_schema(&domain, &mappings).expect("load pokeapi_mini");
    let arc = Arc::new(cgs);
    let disc = TypedDiscovery::from_cgs_entries(vec![("pokeapi_mini".into(), arc)], None)
        .with_max_options(8);

    let out = disc
        .discover(DiscoveryQuery {
            // Use singular "berry" — substring match for catalog token `berry` (plural "berries" is not a substring of `berry`).
            utterance: "list every berry from the api".into(),
            allowed_entry_ids: vec!["pokeapi_mini".into()],
            max_options: 8,
            ..Default::default()
        })
        .await
        .expect("discover");

    match out {
        DiscoveryDecision::Ready { target } => {
            assert_eq!(target.entity, "Berry");
            assert_eq!(target.entry_id, "pokeapi_mini");
        }
        other => panic!("expected Ready, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_utterance_errors() {
    let (domain, mappings) = pokeapi_fixture_paths();
    let cgs = load_split_schema(&domain, &mappings).expect("load pokeapi_mini");
    let disc = TypedDiscovery::from_cgs_entries(vec![("pokeapi_mini".into(), Arc::new(cgs))], None);

    let err = disc
        .discover(DiscoveryQuery {
            utterance: "   ".into(),
            allowed_entry_ids: vec!["pokeapi_mini".into()],
            ..Default::default()
        })
        .await
        .expect_err("empty utterance");
    assert!(matches!(
        err,
        plasm_discovery::DiscoveryError::EmptyUtterance
    ));
}
