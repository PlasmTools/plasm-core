//! Federate catalog rows.

use super::super::super::*;

use super::super::seeds::{normalize_execute_entity_names, wrap_teaching_markdown_literal_block};
use super::exposure_replay::{apply_federate_exposure_wave, ExposureCatalogWave};

/// Append another registry row’s [`plasm_core::CgsContext`] to an existing execute session (same
/// `prompt_hash` / `session`); monotonic `e#` / `m#` / `p#` via [`plasm_core::TeachingExposureSession`].
#[allow(clippy::too_many_arguments)]
pub async fn federate_execute_session(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    new_entry_id: String,
    entities: Vec<String>,
    principal: Option<String>,
    outbound_hosted_kv_by_entry: Option<&HashMap<String, String>>,
    bindings_by_entry: Option<&HashMap<String, crate::binding_slots::SessionBindingMap>>,
) -> Result<CapabilityWaveOutcome, String> {
    let mode = auth_resolution_mode_from_env();
    validate_principal_for_mode(mode, principal.as_deref())?;

    let names = normalize_execute_entity_names(entities);
    if names.is_empty() {
        return Err("`entities` must be non-empty".into());
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
    let scope_intent = sess.context_intent.clone();
    let ranked_slice = sess.ranked_capabilities.as_deref();

    if sess.contexts_by_entry.contains_key(&new_entry_id) {
        return Err(format!(
            "session already includes catalog entry `{new_entry_id}`"
        ));
    }

    let reg = st.catalog.snapshot();
    let registry_pin = reg
        .load_context(&new_entry_id)
        .map(|ctx| ctx.cgs.catalog_cgs_hash_hex())
        .map_err(|e| e.to_string())?;
    let hosted_kv_key = outbound_hosted_kv_by_entry
        .and_then(|map| map.get(&new_entry_id))
        .map(|s| s.as_str());
    let entry_bindings = bindings_by_entry.and_then(|m| m.get(&new_entry_id));
    let materialized = crate::execute_session_materialize::materialize_entry_context(
        st,
        new_entry_id.as_str(),
        hosted_kv_key,
        entry_bindings,
    )
    .await?;
    let ctx_arc = materialized.ctx;

    for e in &names {
        if ctx_arc.get_entity(e).is_none() {
            return Err(format!("unknown entity `{e}` in this schema"));
        }
    }

    sess.contexts_by_entry
        .insert(new_entry_id.clone(), ctx_arc.clone());
    sess.registry_catalog_hashes_by_entry
        .insert(new_entry_id.clone(), registry_pin);
    if let Some(map) = bindings_by_entry {
        if let Some(b) = map.get(&new_entry_id) {
            sess.bindings_by_entry
                .insert(new_entry_id.clone(), b.clone());
        }
    }

    let Some(mut exp) = sess.teaching_exposure.take() else {
        return Err("session has no incremental exposure state".into());
    };

    let slots_before = exp.surface.slots.clone();

    let n0 = exp.entities.len();
    apply_federate_exposure_wave(
        &mut exp,
        &sess
            .contexts_by_entry
            .values()
            .map(|c| c.cgs.as_ref())
            .collect::<Vec<_>>(),
        &sess.contexts_by_entry,
        &ExposureCatalogWave {
            entry_id: new_entry_id.clone(),
            entities: names.clone(),
            read_first_seeded: true,
        },
        scope_intent.as_deref(),
        ranked_slice,
    );
    let added_qualified = exp.qualified_entities_since(n0);
    let new_relation_slots = exp.relation_edge_delta_slots(&slots_before, &added_qualified);
    let layers: Vec<&CGS> = sess
        .contexts_by_entry
        .values()
        .map(|c| c.cgs.as_ref())
        .collect();
    exp.admit_relation_edge_slots_for_render(&layers, &new_relation_slots);
    let relations_delta = exp.relations_delta_rows_for_slots(&new_relation_slots);

    if added_qualified.is_empty() {
        sess.teaching_exposure = Some(exp);
        st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
            .await;
        return Ok(CapabilityWaveOutcome {
            mode: "federate".to_string(),
            entry_id: new_entry_id,
            entities: names,
            markdown_delta: String::new(),
            reused_session: true,
            teaching_prompt_chars_added: 0,
            relations_delta: Vec::new(),
        });
    }

    let by_entry: IndexMap<String, &CGS> = sess
        .contexts_by_entry
        .iter()
        .map(|(k, v)| (k.clone(), v.cgs.as_ref()))
        .collect();
    let sym_cross = st.sessions.symbol_map_cross_cache();
    let delta = st
        .engine
        .prompt_pipeline()
        .render_teaching_exposure_delta_federated_with_edges(
            &by_entry,
            &exp,
            &added_qualified,
            &new_relation_slots,
            Some(sym_cross),
        );
    let wave =
        wrap_teaching_markdown_literal_block(&delta, st.engine.prompt_pipeline().render_mode);
    sess.prompt_text.push_str("\n\n");
    sess.prompt_text.push_str(&wave);
    sess.entities = exp.entities.clone();
    sess.teaching_exposure = Some(exp);
    sess.domain_revision = sess.domain_revision.saturating_add(1);
    st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
        .await;

    Ok(CapabilityWaveOutcome {
        mode: "federate".to_string(),
        entry_id: new_entry_id,
        entities: names,
        markdown_delta: wave.clone(),
        reused_session: false,
        teaching_prompt_chars_added: wave.chars().count() as u64,
        relations_delta,
    })
}
