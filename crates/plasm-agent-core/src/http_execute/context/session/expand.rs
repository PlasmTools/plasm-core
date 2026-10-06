//! Expand teaching waves.

use super::super::super::*;

use super::super::seeds::{
    dedup_preserve_arrival_order, normalize_capability_seeds, process_order_for_expand_group,
};
use crate::session_coordination::ExecuteCoordKey;

/// Markdown delta plus relation-hop metadata from one expand wave.
#[derive(Debug, Clone, Default)]
pub struct ExpandTeachingWaveResult {
    pub markdown: String,
    pub relations_delta: Vec<plasm_core::ExposedRelationSymbolRow>,
}

async fn commit_expand_wave(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    prompt_hash_p: PromptHashHex,
    session_id_p: ExecuteSessionId,
    seeds: Vec<CapabilitySeed>,
) -> Result<ExpandTeachingWaveResult, super::SessionMutateError> {
    let seeds = normalize_capability_seeds(seeds);
    if seeds.is_empty() {
        return Err(super::SessionMutateError::EmptySeeds);
    }

    let Some(sess_arc) = st
        .try_get_execute_session(prompt_hash_p.as_str(), session_id_p.as_str())
        .await?
    else {
        return Err(super::SessionMutateError::UnknownOrExpiredSession);
    };
    let mut sess = (*sess_arc).clone();
    if !session_allows_principal(&sess, principal) {
        return Err(super::SessionMutateError::TenantMismatch);
    }
    let Some(mut exp) = sess.teaching_exposure.take() else {
        return Err(super::SessionMutateError::MissingExposureState);
    };

    let slots_before = exp.surface.slots.clone();
    let caps_before = exp.surface.capabilities.clone();

    let layers: Vec<&CGS> = sess
        .contexts_by_entry
        .values()
        .map(|c| c.cgs.as_ref())
        .collect();
    let n0 = exp.entities.len();
    let mut groups: IndexMap<String, Vec<String>> = IndexMap::new();
    for seed in &seeds {
        let Some(ctx) = sess.contexts_by_entry.get(&seed.entry_id) else {
            return Err(super::SessionMutateError::UnknownCatalogEntry {
                entry_id: seed.entry_id.clone(),
            });
        };
        if ctx.get_entity(&seed.entity).is_none() {
            let hints = crate::http_execute::context::seed_resolve::nearest_entity_names(
                ctx.cgs.as_ref(),
                seed.entity.as_str(),
                5,
            );
            return Err(super::SessionMutateError::UnknownSeedEntity {
                entry_id: seed.entry_id.clone(),
                entity: seed.entity.clone(),
                nearest: hints,
            });
        }
        groups
            .entry(seed.entry_id.clone())
            .or_default()
            .push(seed.entity.clone());
    }
    let mut relation_keys = exp.all_qualified_entities();
    let mut relation_seen: std::collections::BTreeSet<(String, String)> = relation_keys
        .iter()
        .map(|k| (k.entry_id.clone(), k.entity.to_string()))
        .collect();
    for (eid, ents) in &groups {
        for e in ents {
            let pair = (eid.clone(), e.clone());
            if relation_seen.insert(pair.clone()) {
                relation_keys.push(plasm_core::ExposureEntityKey {
                    entry_id: pair.0,
                    entity: plasm_core::EntityName::from(pair.1.as_str()),
                });
            }
        }
    }

    let eid_order = process_order_for_expand_group(&groups);
    for eid in eid_order {
        let Some(ctx) = sess.contexts_by_entry.get(&eid) else {
            return Err(super::SessionMutateError::UnknownCatalogEntry { entry_id: eid });
        };
        let group = groups
            .get(&eid)
            .ok_or_else(|| super::SessionMutateError::MissingSeedGroup {
                entry_id: eid.clone(),
            })?
            .clone();
        let normalized = dedup_preserve_arrival_order(group);
        let refs: Vec<&str> = normalized.iter().map(|s| s.as_str()).collect();
        let delta = st.capability_surface_for_wave(ctx.cgs.as_ref(), &eid, &normalized)?;
        exp.expose_surface(&layers, ctx.cgs.clone(), &eid, &refs, delta);
    }
    let committed = super::commit::commit_exposure_wave_delta(
        st,
        &prompt_hash_p,
        &session_id_p,
        sess,
        exp,
        super::commit::ExposureWaveSnapshot {
            slots_before,
            caps_before,
            entity_count_before: n0,
            relation_keys,
        },
    )
    .await?;
    Ok(ExpandTeachingWaveResult {
        markdown: committed.markdown,
        relations_delta: committed.relations_delta,
    })
}

/// Append expand-wave Plasm instruction blocks for more entity names; [`TeachingExposureSession`] keeps `e#`/`m#`/`p#` stable.
pub async fn expand_execute_teaching_session(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    prompt_hash: &str,
    session_id: &str,
    seeds: Vec<CapabilitySeed>,
) -> Result<ExpandTeachingWaveResult, super::SessionMutateError> {
    let prompt_hash_p: PromptHashHex = prompt_hash
        .parse()
        .map_err(|_| super::SessionMutateError::InvalidPromptHash)?;
    let session_id_p: ExecuteSessionId = session_id
        .parse()
        .map_err(|_| super::SessionMutateError::InvalidSessionId)?;
    let key = ExecuteCoordKey {
        prompt_hash: prompt_hash.to_string(),
        session_id: session_id.to_string(),
    };
    st.session_coordination
        .with_exposure_commit(&key, || async {
            commit_expand_wave(st, principal, prompt_hash_p, session_id_p, seeds).await
        })
        .await
}
