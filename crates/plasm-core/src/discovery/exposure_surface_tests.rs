//! Tests for [`super`] exposure surface derivation and mutator admit gates.

use super::*;
use crate::discovery::score_capability_bm25;
use crate::loader::load_schema_dir;
use crate::symbol_tuning::{ExposureEntityKey, ExposureSlotKey};
use crate::EntityName;
use std::path::Path;

fn relation_keys(entry_id: &str, names: &[&str]) -> Vec<ExposureEntityKey> {
    names
        .iter()
        .map(|n| ExposureEntityKey {
            entry_id: entry_id.to_string(),
            entity: EntityName::from(*n),
        })
        .collect()
}

#[test]
fn intent_surface_omits_relation_until_relation_target_in_scope() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    let cgs = load_schema_dir(&dir).expect("overshow_tools");
    let endpoints = relation_keys("overshow", &["Profile"]);
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "overshow",
        "display profiles",
        &endpoints,
        &["Profile".to_string()],
        None,
        ExposureSurfaceOptions::default(),
    );
    assert!(
        !delta.required.slots.iter().any(|s| matches!(
            s,
            ExposureSlotKey::Relation { relation, .. }
                if relation.as_str() == "recorded_matches"
        )),
        "Profile.recorded_matches targets RecordedContent; omit until that entity is in scope"
    );
}

#[test]
fn intent_surface_includes_profile_relation_when_recorded_content_in_scope() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    let cgs = load_schema_dir(&dir).expect("overshow_tools");
    let endpoints = relation_keys("overshow", &["Profile", "RecordedContent"]);
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "overshow",
        "profile and captured content",
        &endpoints,
        &["Profile".to_string()],
        None,
        ExposureSurfaceOptions::default(),
    );
    assert!(
        delta.required.slots.iter().any(|s| matches!(
            s,
            ExposureSlotKey::Relation { relation, .. }
                if relation.as_str() == "recorded_matches"
        )),
        "expected recorded_matches when RecordedContent is an allowed relation endpoint"
    );
}

#[test]
fn mutating_capability_admitted_requires_nonzero_score() {
    assert!(!mutating_capability_admitted(0, None, "langitem_create"));
    assert!(mutating_capability_admitted(1, None, "langitem_create"));
}

#[test]
fn intent_only_read_surface_excludes_unrequested_mutations() {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
    let endpoints = relation_keys("matrix", &["LangItem"]);
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "matrix",
        "browse language item inventory metadata",
        &endpoints,
        &["LangItem".to_string()],
        None,
        ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::IntentOnly,
        },
    );
    assert!(delta
        .required
        .capabilities
        .iter()
        .filter_map(|key| cgs.get_capability(key.capability.as_str()))
        .any(crate::CapabilitySchema::is_read));
    assert!(!delta
        .required
        .capabilities
        .iter()
        .filter_map(|key| cgs.get_capability(key.capability.as_str()))
        .any(crate::CapabilitySchema::is_remote_mutation));
}

#[test]
fn intent_surface_ranked_admits_seeded_mutator_at_zero_score() {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
    let cap = cgs
        .capabilities
        .get("langitem_create")
        .expect("langitem_create");
    let zero_intent = "xyzzy qwerty plugh unrelated";
    let zero_score = score_capability_bm25(&cgs, "matrix", cap, zero_intent);
    assert_eq!(
        zero_score, 0,
        "fixture intent must score zero for langitem_create"
    );
    let endpoints = relation_keys("matrix", &["LangItem"]);
    let delta_ranked = derive_intent_exposure_surface_batch(
        &cgs,
        "matrix",
        zero_intent,
        &endpoints,
        &["LangItem".to_string()],
        Some(&["langitem_create".to_string()]),
        ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::IntentOnly,
        },
    );
    assert!(
        delta_ranked
            .required
            .capabilities
            .iter()
            .any(|c| { c.capability.as_str() == "langitem_create" }),
        "ranked wire name must admit seeded mutator at score zero under IntentOnly"
    );
}

#[test]
fn intent_surface_ranked_boost_does_not_cage_scored_seeded_mutators() {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
    let endpoints = relation_keys("matrix", &["LangItem"]);
    // Ranked lists an unrelated wire; BM25-scoring create intent must still admit.
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "matrix",
        "create new langitem title",
        &endpoints,
        &["LangItem".to_string()],
        Some(&["some_other_wire".to_string()]),
        ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::IntentOnly,
        },
    );
    assert!(
        delta
            .required
            .capabilities
            .iter()
            .any(|c| c.capability.as_str() == "langitem_create"),
        "ranked list must not cage BM25-scored seeded mutators"
    );
}

#[test]
fn seeded_mutator_admits_ranked_boost_at_zero_score() {
    assert!(!seeded_mutating_capability_admitted(
        0,
        None,
        "langitem_create"
    ));
    assert!(seeded_mutating_capability_admitted(
        0,
        Some(&["langitem_create".to_string()]),
        "langitem_create",
    ));
    assert!(seeded_mutating_capability_admitted(
        1,
        None,
        "langitem_create"
    ));
}

#[test]
fn intent_surface_always_on_seeds_admits_seeded_mutators_on_first_wave() {
    let dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/plasm_language_matrix");
    let cgs = load_schema_dir(&dir).expect("plasm_language_matrix");
    let cap = cgs
        .capabilities
        .get("langitem_create")
        .expect("langitem_create");
    let weak_intent = "langitem browse inventory metadata";
    let strong_intent = "create new langitem title";
    let weak_score = score_capability_bm25(&cgs, "matrix", cap, weak_intent);
    let strong_score = score_capability_bm25(&cgs, "matrix", cap, strong_intent);
    assert!(
        strong_score > 0,
        "strong create intent should BM25-score: {strong_score}"
    );
    assert!(
        strong_score >= weak_score,
        "strong intent should score at least as high as weak: {strong_score} vs {weak_score}"
    );
    let endpoints = relation_keys("matrix", &["LangItem"]);
    let delta_weak = derive_intent_exposure_surface_batch(
        &cgs,
        "matrix",
        weak_intent,
        &endpoints,
        &["LangItem".to_string()],
        None,
        ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
        },
    );
    assert!(
        delta_weak
            .required
            .capabilities
            .iter()
            .any(|c| { c.capability.as_str() == "langitem_create" }),
        "read-first should admit seeded mutators even at weak intent score"
    );
    let delta_strong = derive_intent_exposure_surface_batch(
        &cgs,
        "matrix",
        strong_intent,
        &endpoints,
        &["LangItem".to_string()],
        None,
        ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
        },
    );
    assert!(
        delta_strong
            .required
            .capabilities
            .iter()
            .any(|c| { c.capability.as_str() == "langitem_create" }),
        "read-first should admit strong-scored seeded mutator"
    );
    let delta_ranked = derive_intent_exposure_surface_batch(
        &cgs,
        "matrix",
        weak_intent,
        &endpoints,
        &["LangItem".to_string()],
        Some(&["langitem_create".to_string()]),
        ExposureSurfaceOptions {
            mutator_admit: MutatorAdmit::AlwaysOnSeeds,
        },
    );
    assert!(
        delta_ranked
            .required
            .capabilities
            .iter()
            .any(|c| { c.capability.as_str() == "langitem_create" }),
        "read-first should admit ranked mutator wire name"
    );
}

#[test]
fn intent_surface_seeded_prompt_run_create_requires_intent_overlap() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    let cgs = load_schema_dir(&dir).expect("overshow_tools");
    let endpoints = relation_keys("overshow", &["PromptRun"]);
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "overshow",
        "list profiles read metadata only",
        &endpoints,
        &["PromptRun".to_string()],
        None,
        ExposureSurfaceOptions::default(),
    );
    assert!(
        !delta
            .required
            .capabilities
            .iter()
            .any(|c| c.capability.as_str() == "prompt_run_create"),
        "seeded PromptRun create must require intent overlap"
    );
    let delta_create = derive_intent_exposure_surface_batch(
        &cgs,
        "overshow",
        "create and execute a new prompt run",
        &endpoints,
        &["PromptRun".to_string()],
        None,
        ExposureSurfaceOptions::default(),
    );
    assert!(
        delta_create
            .required
            .capabilities
            .iter()
            .any(|c| c.capability.as_str() == "prompt_run_create"),
        "seeded PromptRun create should appear when intent scores it"
    );
}

#[test]
fn intent_surface_drops_unscored_reads_when_intent_targets_other_entity() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    let cgs = load_schema_dir(&dir).expect("overshow_tools");
    let endpoints = relation_keys("overshow", &["Meeting", "Profile"]);
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "overshow",
        "organisation project profile metadata list",
        &endpoints,
        &["Profile".to_string()],
        None,
        ExposureSurfaceOptions::default(),
    );
    assert!(
        delta.required.capabilities.iter().any(|c| {
            c.domain.as_str() == "Profile"
                && matches!(c.capability.as_str(), "profile_query" | "profile_get")
        }),
        "expected Profile query/get to remain when intent lexicon scores profile vocabulary"
    );
    assert!(
        !delta.required.capabilities.iter().any(|c| {
            c.domain.as_str() == "Meeting"
                && matches!(c.capability.as_str(), "meeting_query" | "meeting_get")
        }),
        "Meeting reads should be omitted when intent does not score meeting vocabulary"
    );
}

#[cfg(feature = "ranked_capability_gate")]
#[test]
fn intent_surface_ranked_gate_excludes_non_seeded_scored_mutation() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    let cgs = load_schema_dir(&dir).expect("overshow_tools");
    let endpoints = relation_keys("overshow", &["PromptRun", "Profile"]);
    let ranked = vec!["prompt_run_create".to_string()];
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "overshow",
        "create and execute a new prompt run",
        &endpoints,
        &["Profile".to_string()],
        Some(&ranked),
        ExposureSurfaceOptions::default(),
    );
    assert!(
        !surface_has_capability(&delta, "PromptRun", "prompt_run_create"),
        "PromptRun create must stay off surface when PromptRun is not seeded (ranked list alone does not add caps)"
    );
}

#[cfg(feature = "ranked_capability_gate")]
#[test]
fn intent_surface_ranked_gate_keeps_mutation_on_list() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
    let cgs = load_schema_dir(&dir).expect("overshow_tools");
    let endpoints = relation_keys("overshow", &["PromptRun"]);
    let ranked = vec!["prompt_run_create".to_string()];
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "overshow",
        "create and execute a new prompt run",
        &endpoints,
        &["PromptRun".to_string()],
        Some(&ranked),
        ExposureSurfaceOptions::default(),
    );
    assert!(
        delta.required.capabilities.iter().any(|c| {
            c.capability.as_str() == "prompt_run_create"
        }),
        "ranked gate should admit mutations present in the ranked name list when intent scores them"
    );
}

const FEDERATED_FIELD_LAB_INTENT: &str =
    "Federated field lab v2 — pokeapi specimen linear missions proof dossier";

fn surface_has_capability(delta: &ExposureSurfaceDelta, domain: &str, capability: &str) -> bool {
    delta
        .required
        .capabilities
        .iter()
        .any(|c| c.domain.as_str() == domain && c.capability.as_str() == capability)
}

#[test]
fn intent_surface_seeded_sharelink_create_requires_intent_overlap() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
    if !dir.is_dir() {
        return;
    }
    let mut cgs = load_schema_dir(&dir).expect("proof");
    cgs.entry_id = Some("proof".into());
    let endpoints = relation_keys("proof", &["ShareLink"]);
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "proof",
        FEDERATED_FIELD_LAB_INTENT,
        &endpoints,
        &["ShareLink".to_string()],
        None,
        ExposureSurfaceOptions::default(),
    );
    assert!(
        !surface_has_capability(&delta, "ShareLink", "share_link_create"),
        "seeded create must require intent overlap when intent omits share/link/create tokens"
    );
    let delta_create = derive_intent_exposure_surface_batch(
        &cgs,
        "proof",
        "create share link for proof dossier",
        &endpoints,
        &["ShareLink".to_string()],
        None,
        ExposureSurfaceOptions::default(),
    );
    assert!(
        surface_has_capability(&delta_create, "ShareLink", "share_link_create"),
        "seeded create should appear when intent scores the mutation"
    );
}

#[test]
fn intent_surface_seeded_sharelink_create_with_intent_lexicon_match() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
    if !dir.is_dir() {
        return;
    }
    let mut cgs = load_schema_dir(&dir).expect("proof");
    cgs.entry_id = Some("proof".into());
    let endpoints = relation_keys("proof", &["ShareLink"]);
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "proof",
        "create share link for proof dossier",
        &endpoints,
        &["ShareLink".to_string()],
        None,
        ExposureSurfaceOptions::default(),
    );
    assert!(
        surface_has_capability(&delta, "ShareLink", "share_link_create"),
        "seeded ShareLink must expose share_link_create when intent scores create"
    );
    let session = crate::symbol_tuning::TeachingExposureSession::new_with_intent_delta(
        &cgs,
        "proof",
        &["ShareLink"],
        delta,
    );
    let map = session.to_symbol_map();
    let m = map.method_sym_for("proof", "ShareLink", "share_link_create");
    assert!(
        m.starts_with('m') && m.len() > 1,
        "seeded share_link_create must receive an m# (got {m:?}) for federated lab plasm programs"
    );
}

#[test]
fn intent_surface_seeded_pokemon_reads_without_intent_lexicon_match() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).expect("pokeapi");
    let endpoints = relation_keys("pokeapi", &["Pokemon"]);
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "pokeapi",
        FEDERATED_FIELD_LAB_INTENT,
        &endpoints,
        &["Pokemon".to_string()],
        None,
        ExposureSurfaceOptions::default(),
    );
    assert!(
        surface_has_capability(&delta, "Pokemon", "pokemon_query"),
        "seeded Pokemon must expose pokemon_query"
    );
    assert!(
        surface_has_capability(&delta, "Pokemon", "pokemon_get"),
        "seeded Pokemon must expose pokemon_get"
    );
}

#[cfg(feature = "ranked_capability_gate")]
#[test]
fn intent_surface_ranked_list_does_not_cage_scored_seeded_create() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
    if !dir.is_dir() {
        return;
    }
    let cgs = load_schema_dir(&dir).expect("proof");
    let endpoints = relation_keys("proof", &["ShareLink"]);
    let ranked = vec!["__not_share_link_create__".to_string()];
    let delta = derive_intent_exposure_surface_batch(
        &cgs,
        "proof",
        "create share link for proof dossier",
        &endpoints,
        &["ShareLink".to_string()],
        Some(&ranked),
        ExposureSurfaceOptions::default(),
    );
    assert!(
        surface_has_capability(&delta, "ShareLink", "share_link_create"),
        "ranked list must not cage BM25-scored seeded mutators"
    );
}
