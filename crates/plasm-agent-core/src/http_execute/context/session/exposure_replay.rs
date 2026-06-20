//! Canonical teaching-exposure wave replay (live federate + cross-pod rehydrate).

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::{IndexMap, IndexSet};
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
    let mut catalog_order: IndexSet<String> = IndexSet::new();
    let mut entities_by_catalog: HashMap<String, Vec<String>> = HashMap::new();
    for (entity, entry_id) in entities.iter().zip(entity_catalog_entry_ids.iter()) {
        catalog_order.insert(entry_id.clone());
        entities_by_catalog
            .entry(entry_id.clone())
            .or_default()
            .push(entity.clone());
    }
    catalog_order
        .into_iter()
        .enumerate()
        .map(|(i, entry_id)| ExposureCatalogWave {
            entities: entities_by_catalog
                .get(&entry_id)
                .cloned()
                .unwrap_or_default(),
            entry_id,
            read_first_seeded: i > 0,
        })
        .collect()
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
    use plasm_core::{load_schema_dir, PromptPipelineConfig};
    use std::sync::Arc;

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
}
