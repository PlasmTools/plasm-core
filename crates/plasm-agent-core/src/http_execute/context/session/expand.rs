//! Expand teaching waves.

use super::super::super::*;

use super::super::seeds::{
    normalize_execute_entity_names, process_order_for_expand_group,
    wrap_teaching_markdown_literal_block,
};

/// Markdown delta plus relation-hop metadata from one expand wave.
#[derive(Debug, Clone, Default)]
pub struct ExpandTeachingWaveResult {
    pub markdown: String,
    pub relations_delta: Vec<plasm_core::ExposedRelationSymbolRow>,
}

/// Append expand-wave Plasm instruction blocks for more entity names; [`TeachingExposureSession`] keeps `e#`/`m#`/`p#` stable.
pub async fn expand_execute_teaching_session(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    prompt_hash: &str,
    session_id: &str,
    seeds: Vec<CapabilitySeed>,
) -> Result<ExpandTeachingWaveResult, String> {
    let seeds = normalize_capability_seeds(seeds);
    if seeds.is_empty() {
        return Err("`seeds` must be non-empty".into());
    }
    let prompt_hash_p: PromptHashHex = prompt_hash
        .parse()
        .map_err(|e: &'static str| e.to_string())?;
    let session_id_p: ExecuteSessionId = session_id
        .parse()
        .map_err(|e: &'static str| e.to_string())?;

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
    let ranked_slice = sess.ranked_capabilities.as_deref();
    let Some(mut exp) = sess.teaching_exposure.take() else {
        return Err("session has no incremental exposure state".into());
    };

    let slots_before = exp.surface.slots.clone();

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
        let normalized = normalize_execute_entity_names(group);
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
    let added_qualified = exp.qualified_entities_since(n0);
    let added: Vec<&str> = added_qualified.iter().map(|k| k.entity.as_str()).collect();
    let new_relation_slots = exp.relation_edge_delta_slots(&slots_before, &added_qualified);
    exp.admit_relation_edge_slots_for_render(&layers, &new_relation_slots);
    let relations_delta = exp.relations_delta_rows_for_slots(&new_relation_slots);

    if added_qualified.is_empty() {
        sess.entities = exp.entities.clone();
        sess.teaching_exposure = Some(exp);
        st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
            .await;
        return Ok(ExpandTeachingWaveResult::default());
    }

    let cgs_primary = sess.cgs.as_ref();
    let sym_cross = st.sessions.symbol_map_cross_cache();
    let delta = if sess.contexts_by_entry.len() > 1 {
        let by_entry: IndexMap<String, &CGS> = sess
            .contexts_by_entry
            .iter()
            .map(|(k, v)| (k.clone(), v.cgs.as_ref()))
            .collect();
        st.engine
            .prompt_pipeline()
            .render_teaching_exposure_delta_federated_with_edges(
                &by_entry,
                &exp,
                &added_qualified,
                &new_relation_slots,
                Some(sym_cross),
            )
    } else {
        st.engine.prompt_pipeline().render_teaching_exposure_delta_with_edges(
            cgs_primary,
            &exp,
            &added,
            &new_relation_slots,
            Some(sym_cross),
        )
    };
    let wave =
        wrap_teaching_markdown_literal_block(&delta, st.engine.prompt_pipeline().render_mode);
    sess.prompt_text.push_str("\n\n");
    sess.prompt_text.push_str(&wave);
    sess.entities = exp.entities.clone();
    sess.teaching_exposure = Some(exp);
    sess.domain_revision = sess.domain_revision.saturating_add(1);
    st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
        .await;
    Ok(ExpandTeachingWaveResult {
        markdown: wave,
        relations_delta,
    })
}
