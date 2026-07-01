//! Canonical teaching-exposure wave replay (live federate + cross-pod rehydrate).

use std::sync::Arc;

use indexmap::IndexMap;
use plasm_core::discovery::ExposureSurfaceOptions;
use plasm_core::{CgsContext, TeachingExposureSession, CGS};

use super::super::seeds::relation_endpoint_keys_for_wave;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposureCatalogWave {
    pub entry_id: String,
    pub entities: Vec<String>,
    /// Federate follow-up waves use `true`; first open/rehydrate primary wave uses `false`.
    pub read_first_seeded: bool,
}

pub fn catalog_waves_from_pairing(
    entities: &[String],
    entity_catalog_entry_ids: &[String],
) -> Vec<ExposureCatalogWave> {
    if entities.is_empty() {
        return Vec::new();
    }
    assert_eq!(
        entities.len(),
        entity_catalog_entry_ids.len(),
        "entity/catalog pairing must be validated before wave derivation"
    );
    let mut waves = Vec::new();
    let mut start = 0;
    while start < entities.len() {
        let entry_id = entity_catalog_entry_ids[start].clone();
        let mut end = start + 1;
        while end < entities.len() && entity_catalog_entry_ids[end] == entry_id {
            end += 1;
        }
        waves.push(ExposureCatalogWave {
            entry_id,
            entities: entities[start..end].to_vec(),
            // Match live federate: first wave is primary open (`read_first_seeded: false`);
            // every subsequent wave (even same catalog re-entry) is incremental.
            read_first_seeded: !waves.is_empty(),
        });
        start = end;
    }
    waves
}

pub fn build_initial_exposure_wave(
    contexts_by_entry: &IndexMap<String, Arc<CgsContext>>,
    wave: &ExposureCatalogWave,
    context_intent: Option<&str>,
    ranked_capabilities: Option<&[String]>,
) -> TeachingExposureSession {
    let cgs = contexts_by_entry
        .get(&wave.entry_id)
        .map(|c| c.cgs.as_ref())
        .expect("catalog context must exist for initial exposure wave");
    let refs: Vec<&str> = wave.entities.iter().map(String::as_str).collect();
    match context_intent {
        Some(intent_s) => {
            let relation_keys =
                plasm_core::relation_endpoint_keys(wave.entry_id.as_str(), &wave.entities);
            let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
                cgs,
                wave.entry_id.as_str(),
                intent_s,
                &relation_keys,
                &wave.entities,
                ranked_capabilities,
                ExposureSurfaceOptions {
                    read_first_seeded: wave.read_first_seeded,
                },
            );
            TeachingExposureSession::new_with_intent_delta(
                cgs,
                wave.entry_id.as_str(),
                &refs,
                delta,
            )
        }
        None => TeachingExposureSession::new(cgs, wave.entry_id.as_str(), &refs),
    }
}

pub fn apply_federate_exposure_wave(
    exp: &mut TeachingExposureSession,
    layers: &[&CGS],
    contexts_by_entry: &IndexMap<String, Arc<CgsContext>>,
    wave: &ExposureCatalogWave,
    context_intent: Option<&str>,
    ranked_capabilities: Option<&[String]>,
) {
    let ctx = contexts_by_entry
        .get(&wave.entry_id)
        .expect("catalog context must exist for exposure wave");
    let refs: Vec<&str> = wave.entities.iter().map(String::as_str).collect();
    if let Some(intent_s) = context_intent {
        let relation_keys = if exp.entities.is_empty() {
            plasm_core::relation_endpoint_keys(wave.entry_id.as_str(), &wave.entities)
        } else {
            relation_endpoint_keys_for_wave(exp, wave.entry_id.as_str(), &wave.entities)
        };
        let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
            ctx.cgs.as_ref(),
            wave.entry_id.as_str(),
            intent_s,
            &relation_keys,
            &wave.entities,
            ranked_capabilities,
            ExposureSurfaceOptions {
                read_first_seeded: wave.read_first_seeded,
            },
        );
        exp.expose_surface(
            layers,
            ctx.cgs.clone(),
            wave.entry_id.as_str(),
            &refs,
            delta,
        );
    } else {
        exp.expose_entities(layers, ctx.cgs.clone(), wave.entry_id.as_str(), &refs);
    }
}

pub fn replay_teaching_exposure_waves(
    contexts_by_entry: &IndexMap<String, Arc<CgsContext>>,
    entities: &[String],
    entity_catalog_entry_ids: &[String],
    context_intent: Option<&str>,
    ranked_capabilities: Option<&[String]>,
) -> TeachingExposureSession {
    let waves = catalog_waves_from_pairing(entities, entity_catalog_entry_ids);
    let Some(first) = waves.first() else {
        let entry_id = contexts_by_entry
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "default".into());
        return TeachingExposureSession::new(
            contexts_by_entry
                .get(&entry_id)
                .map(|c| c.cgs.as_ref())
                .expect("catalog context"),
            entry_id.as_str(),
            &[],
        );
    };
    let layers: Vec<&CGS> = contexts_by_entry.values().map(|c| c.cgs.as_ref()).collect();
    let mut exp = build_initial_exposure_wave(
        contexts_by_entry,
        first,
        context_intent,
        ranked_capabilities,
    );
    for wave in waves.iter().skip(1) {
        apply_federate_exposure_wave(
            &mut exp,
            &layers,
            contexts_by_entry,
            wave,
            context_intent,
            ranked_capabilities,
        );
    }
    exp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::exposure_replay_fixtures::{
        assert_github_langdetail_numbering_parity, interleaved_federated_matrix_fixture,
    };
    use plasm_core::{load_schema_dir, CgsContext, PromptPipelineConfig, CGS};
    use std::sync::Arc;

    #[test]
    fn catalog_waves_from_pairing_preserves_interleaved_catalog_runs() {
        let entities = vec![
            "LangItem".to_string(),
            "LangDetail".to_string(),
            "LangTag".to_string(),
        ];
        let catalog_ids = vec![
            "linear".to_string(),
            "github".to_string(),
            "linear".to_string(),
        ];
        let waves = catalog_waves_from_pairing(&entities, &catalog_ids);
        assert_eq!(waves.len(), 3);
        assert_eq!(waves[0].entry_id, "linear");
        assert_eq!(waves[0].entities, vec!["LangItem"]);
        assert!(!waves[0].read_first_seeded);
        assert_eq!(waves[1].entry_id, "github");
        assert_eq!(waves[1].entities, vec!["LangDetail"]);
        assert!(waves[1].read_first_seeded);
        assert_eq!(waves[2].entry_id, "linear");
        assert_eq!(waves[2].entities, vec!["LangTag"]);
        assert!(waves[2].read_first_seeded);
    }

    #[test]
    fn interleaved_replay_matches_live_entity_numbering() {
        let fixture = interleaved_federated_matrix_fixture();
        let replay = replay_teaching_exposure_waves(
            &fixture.contexts,
            &fixture.pairing.entities,
            &fixture.pairing.catalog_entry_ids,
            None,
            None,
        );
        assert_github_langdetail_numbering_parity(
            &fixture.live.symbol_map_arc(),
            &replay.symbol_map_arc(),
        );
    }

    #[test]
    fn expand_wave_teaches_berry_firmness_hop_via_exposure_replay() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        if !dir.is_dir() {
            return;
        }
        let cgs = Arc::new(load_schema_dir(&dir).expect("pokeapi schema"));
        let mut contexts = IndexMap::new();
        contexts.insert(
            "pokeapi".to_string(),
            Arc::new(CgsContext::entry("pokeapi", cgs.clone())),
        );
        let intent = "cheri berry firmness";
        let mut exp = build_initial_exposure_wave(
            &contexts,
            &ExposureCatalogWave {
                entry_id: "pokeapi".to_string(),
                entities: vec!["Berry".to_string()],
                read_first_seeded: false,
            },
            Some(intent),
            None,
        );
        let slots_before = exp.surface.slots.clone();
        let n0 = exp.entities.len();
        apply_federate_exposure_wave(
            &mut exp,
            &[cgs.as_ref()],
            &contexts,
            &ExposureCatalogWave {
                entry_id: "pokeapi".to_string(),
                entities: vec!["BerryFirmness".to_string()],
                read_first_seeded: true,
            },
            Some(intent),
            None,
        );
        let added = exp.qualified_entities_since(n0);
        let relation_keys =
            exp.relation_endpoint_keys_for_wave("pokeapi", &["BerryFirmness".to_string()]);
        let edge_slots = exp.relation_slots_for_expand_wave(&slots_before, &added, &relation_keys);
        exp.admit_relation_edge_slots_for_render(&[cgs.as_ref()], &edge_slots);
        let pipeline = PromptPipelineConfig::default();
        let delta = pipeline.render_teaching_exposure_delta_with_edges(
            cgs.as_ref(),
            &exp,
            &["BerryFirmness"],
            &edge_slots,
            None,
        );
        assert!(
            delta.contains("relation e1 → e2"),
            "http-style expand delta should teach Berry→BerryFirmness hop: {delta}"
        );
        assert!(
            delta.contains(".r"),
            "delta should include r# symbol: {delta}"
        );
    }

    #[test]
    fn append_only_preserves_label_e4_when_issue_comment_arrives_late() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
        if !dir.is_dir() {
            return;
        }
        let cgs = Arc::new(load_schema_dir(&dir).expect("github"));
        let mut contexts = IndexMap::new();
        contexts.insert(
            "github".to_string(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        let layers: Vec<&CGS> = contexts.values().map(|c| c.cgs.as_ref()).collect();
        let mut exp = build_initial_exposure_wave(
            &contexts,
            &ExposureCatalogWave {
                entry_id: "github".to_string(),
                entities: vec!["Repository".into()],
                read_first_seeded: false,
            },
            Some("label documentation workflow"),
            None,
        );
        apply_federate_exposure_wave(
            &mut exp,
            &layers,
            &contexts,
            &ExposureCatalogWave {
                entry_id: "github".to_string(),
                entities: vec![
                    "Issue".into(),
                    "Label".into(),
                    "PullRequest".into(),
                    "Branch".into(),
                ],
                read_first_seeded: true,
            },
            Some("label documentation workflow"),
            None,
        );
        let label_before = exp
            .qualified_entity_symbol("github", "Label")
            .expect("Label exposed");
        assert_eq!(
            label_before, "e3",
            "Label should be e3 after first expand wave"
        );
        apply_federate_exposure_wave(
            &mut exp,
            &layers,
            &contexts,
            &ExposureCatalogWave {
                entry_id: "github".to_string(),
                entities: vec!["IssueComment".into()],
                read_first_seeded: true,
            },
            Some("label documentation workflow"),
            None,
        );
        assert_eq!(
            exp.qualified_entity_symbol("github", "Label"),
            Some(label_before.to_string()),
            "Label e# must not shift when IssueComment appends"
        );
        assert_eq!(
            exp.qualified_entity_symbol("github", "IssueComment"),
            Some("e6".into()),
            "IssueComment should append as next index"
        );
    }

    #[test]
    fn persisted_ledger_hydrate_preserves_append_only_github_symbols() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/github");
        if !dir.is_dir() {
            return;
        }
        let cgs = Arc::new(load_schema_dir(&dir).expect("github"));
        let mut contexts = IndexMap::new();
        contexts.insert(
            "github".to_string(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        let layers: Vec<&CGS> = contexts.values().map(|c| c.cgs.as_ref()).collect();
        let mut exp = build_initial_exposure_wave(
            &contexts,
            &ExposureCatalogWave {
                entry_id: "github".to_string(),
                entities: vec!["Repository".into()],
                read_first_seeded: false,
            },
            Some("label documentation workflow"),
            None,
        );
        apply_federate_exposure_wave(
            &mut exp,
            &layers,
            &contexts,
            &ExposureCatalogWave {
                entry_id: "github".to_string(),
                entities: vec![
                    "Issue".into(),
                    "Label".into(),
                    "PullRequest".into(),
                    "Branch".into(),
                ],
                read_first_seeded: true,
            },
            Some("label documentation workflow"),
            None,
        );
        apply_federate_exposure_wave(
            &mut exp,
            &layers,
            &contexts,
            &ExposureCatalogWave {
                entry_id: "github".to_string(),
                entities: vec!["IssueComment".into()],
                read_first_seeded: true,
            },
            Some("label documentation workflow"),
            None,
        );
        let hashes = plasm_core::catalog_cgs_hashes_from_session(&exp);
        let snap =
            plasm_core::PersistedSymbolLedger::from_session(&exp, hashes).expect("from_session");
        let bytes = snap.encode().expect("encode");
        let decoded = plasm_core::PersistedSymbolLedger::decode(&bytes).expect("decode");
        let mut catalog_cgs = IndexMap::new();
        catalog_cgs.insert("github".to_string(), cgs);
        let restored = decoded.hydrate(&catalog_cgs).expect("hydrate");
        assert_eq!(
            exp.qualified_entity_symbol("github", "Label"),
            restored.qualified_entity_symbol("github", "Label")
        );
        assert_eq!(
            exp.qualified_entity_symbol("github", "IssueComment"),
            restored.qualified_entity_symbol("github", "IssueComment")
        );
    }

    #[test]
    fn replay_federated_extend_preserves_e4_for_second_catalog_entity() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = Arc::new(load_schema_dir(&dir).expect("matrix"));
        let mut contexts = IndexMap::new();
        contexts.insert(
            "linear".to_string(),
            Arc::new(plasm_core::CgsContext::entry("linear", cgs.clone())),
        );
        contexts.insert(
            "github".to_string(),
            Arc::new(plasm_core::CgsContext::entry("github", cgs.clone())),
        );
        const INTENT: &str = "matrix federated extend e4 compile parity";
        let exp = replay_teaching_exposure_waves(
            &contexts,
            &[
                "LangItem".into(),
                "LangLine".into(),
                "LangTag".into(),
                "LangDetail".into(),
            ],
            &[
                "linear".into(),
                "linear".into(),
                "linear".into(),
                "github".into(),
            ],
            Some(INTENT),
            None,
        );
        let map = exp.symbol_map_arc();
        assert_eq!(
            map.entity_sym_for("github", "LangDetail"),
            "e4",
            "rehydrate replay must assign e4 to github LangDetail"
        );
        assert!(
            map.resolve_session_entity_symbol("e4").is_some(),
            "rehydrated symbol map must resolve opaque e4"
        );
    }
}
