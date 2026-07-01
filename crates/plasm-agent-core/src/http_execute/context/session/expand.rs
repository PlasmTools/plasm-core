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
) -> Result<ExpandTeachingWaveResult, String> {
    let seeds = normalize_capability_seeds(seeds);
    if seeds.is_empty() {
        return Err("`seeds` must be non-empty".into());
    }

    let Some(sess_arc) = st
        .get_execute_session(prompt_hash_p.as_str(), session_id_p.as_str())
        .await
    else {
        return Err("unknown or expired execute session".into());
    };
    let mut sess = (*sess_arc).clone();
    if !session_allows_principal(&sess, principal) {
        return Err("forbidden: execute session tenant does not match caller".into());
    }
    let scope_intent = sess.context_intent.clone();
    let ranked_names = sess.ranked_capabilities.clone();
    let ranked_slice = ranked_names.as_deref();
    let Some(mut exp) = sess.teaching_exposure.take() else {
        return Err("session has no incremental exposure state".into());
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
            return Err(format!(
                "unknown catalog entry `{}` in loaded session schemas",
                seed.entry_id
            ));
        };
        if ctx.get_entity(&seed.entity).is_none() {
            return Err(format!(
                "unknown entity `{}` in catalog `{}`",
                seed.entity, seed.entry_id
            ));
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
            return Err(format!(
                "unknown catalog entry `{eid}` in loaded session schemas"
            ));
        };
        let group = groups
            .get(&eid)
            .ok_or_else(|| format!("internal error: missing seed group for `{eid}`"))?
            .clone();
        let normalized = dedup_preserve_arrival_order(group);
        let refs: Vec<&str> = normalized.iter().map(|s| s.as_str()).collect();
        if let Some(ref intent_s) = scope_intent {
            let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
                ctx.cgs.as_ref(),
                eid.as_str(),
                intent_s.as_str(),
                &relation_keys,
                &normalized,
                ranked_slice,
                plasm_core::discovery::ExposureSurfaceOptions {
                    read_first_seeded: true,
                },
            );
            exp.expose_surface(&layers, ctx.cgs.clone(), eid.as_str(), &refs, delta);
        } else {
            exp.expose_entities(&layers, ctx.cgs.clone(), eid.as_str(), &refs);
        }
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
            ranked_capability_names: ranked_names,
        },
    )
    .await;
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
) -> Result<ExpandTeachingWaveResult, String> {
    let prompt_hash_p: PromptHashHex = prompt_hash
        .parse()
        .map_err(|e: &'static str| e.to_string())?;
    let session_id_p: ExecuteSessionId = session_id
        .parse()
        .map_err(|e: &'static str| e.to_string())?;
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
