//! Explicit execution wave construction used by fixture and local execution callers.
//! Routed sessions persist their exact capability surface in the symbol ledger.

use indexmap::IndexMap;
use plasm_core::{CgsContext, TeachingExposureSession, CGS};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExposureCatalogWave {
    pub entry_id: String,
    pub entities: Vec<String>,
}

pub fn build_initial_exposure_wave(
    contexts: &IndexMap<String, Arc<CgsContext>>,
    wave: &ExposureCatalogWave,
) -> TeachingExposureSession {
    let cgs = &contexts
        .get(&wave.entry_id)
        .expect("explicit catalog context")
        .cgs;
    let refs = wave.entities.iter().map(String::as_str).collect::<Vec<_>>();
    let delta = plasm_core::capability_exposure::explicit_entity_capability_surface(
        cgs,
        &wave.entry_id,
        &wave.entities,
    )
    .expect("valid explicit capabilities");
    TeachingExposureSession::new_with_intent_delta(cgs, &wave.entry_id, &refs, delta)
}

pub fn apply_federate_exposure_wave(
    exposure: &mut TeachingExposureSession,
    layers: &[&CGS],
    contexts: &IndexMap<String, Arc<CgsContext>>,
    wave: &ExposureCatalogWave,
) {
    let cgs = &contexts
        .get(&wave.entry_id)
        .expect("explicit catalog context")
        .cgs;
    let refs = wave.entities.iter().map(String::as_str).collect::<Vec<_>>();
    let delta = plasm_core::capability_exposure::explicit_entity_capability_surface(
        cgs,
        &wave.entry_id,
        &wave.entities,
    )
    .expect("valid explicit capabilities");
    exposure.expose_surface(layers, cgs.clone(), &wave.entry_id, &refs, delta);
}

#[cfg(test)]
pub fn catalog_waves_from_pairing(
    entities: &[String],
    entries: &[String],
) -> Vec<ExposureCatalogWave> {
    assert_eq!(entities.len(), entries.len());
    let mut waves: Vec<ExposureCatalogWave> = Vec::new();
    for (entity, entry) in entities.iter().zip(entries) {
        if let Some(wave) = waves.last_mut().filter(|wave| wave.entry_id == *entry) {
            wave.entities.push(entity.clone());
        } else {
            waves.push(ExposureCatalogWave {
                entry_id: entry.clone(),
                entities: vec![entity.clone()],
            });
        }
    }
    waves
}

#[cfg(test)]
pub fn replay_teaching_exposure_waves(
    contexts: &IndexMap<String, Arc<CgsContext>>,
    entities: &[String],
    entries: &[String],
) -> TeachingExposureSession {
    let waves = catalog_waves_from_pairing(entities, entries);
    let first = waves.first().expect("explicit replay requires a wave");
    let mut exposure = build_initial_exposure_wave(contexts, first);
    let layers = contexts
        .values()
        .map(|context| context.cgs.as_ref())
        .collect::<Vec<_>>();
    for wave in &waves[1..] {
        apply_federate_exposure_wave(&mut exposure, &layers, contexts, wave);
    }
    exposure
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interleaved_catalog_waves_preserve_arrival_order() {
        let waves = catalog_waves_from_pairing(
            &["A".into(), "B".into(), "C".into()],
            &["left".into(), "right".into(), "left".into()],
        );
        assert_eq!(
            waves
                .iter()
                .map(|wave| wave.entry_id.as_str())
                .collect::<Vec<_>>(),
            ["left", "right", "left"]
        );
    }
}
