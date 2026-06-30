//! Federate catalog rows.

use super::super::super::*;

use super::super::seeds::normalize_execute_entity_names;
use super::exposure_replay::{apply_federate_exposure_wave, ExposureCatalogWave};
use crate::session_coordination::ExecuteCoordKey;

/// Federate materialization completed outside exposure commit gates (I/O allowed).
pub struct PreparedFederateWave {
    pub ctx_arc: Arc<CgsContext>,
    pub registry_pin: String,
    pub new_entry_id: String,
    pub names: Vec<String>,
    pub entry_bindings: Option<crate::binding_slots::SessionBindingMap>,
}

#[allow(clippy::too_many_arguments)]
pub async fn prepare_federate_wave(
    st: &PlasmHostState,
    new_entry_id: String,
    entities: Vec<String>,
    principal: Option<String>,
    outbound_hosted_kv_by_entry: Option<&HashMap<String, String>>,
    bindings_by_entry: Option<&HashMap<String, crate::binding_slots::SessionBindingMap>>,
) -> Result<PreparedFederateWave, String> {
    let mode = auth_resolution_mode_from_env();
    validate_principal_for_mode(mode, principal.as_deref())?;

    let names = normalize_execute_entity_names(entities);
    if names.is_empty() {
        return Err("`entities` must be non-empty".into());
    }

    let reg = st.catalog.snapshot();
    let registry_pin = reg
        .load_context(&new_entry_id)
        .map(|ctx| ctx.cgs.catalog_cgs_hash_hex())
        .map_err(|e| e.to_string())?;
    let hosted_kv_key = outbound_hosted_kv_by_entry
        .and_then(|map| map.get(&new_entry_id))
        .map(|s| s.as_str());
    let entry_bindings = bindings_by_entry
        .and_then(|m| m.get(&new_entry_id))
        .cloned();
    let materialized = crate::execute_session_materialize::materialize_entry_context(
        st,
        new_entry_id.as_str(),
        hosted_kv_key,
        entry_bindings.as_ref(),
    )
    .await?;
    let ctx_arc = materialized.ctx;

    for e in &names {
        if ctx_arc.get_entity(e).is_none() {
            return Err(format!("unknown entity `{e}` in this schema"));
        }
    }

    Ok(PreparedFederateWave {
        ctx_arc,
        registry_pin,
        new_entry_id,
        names,
        entry_bindings,
    })
}

/// Commit a prepared federate wave under the per-execute-row exposure-commit gate (CEP-13). The
/// caller principal was already validated in [`prepare_federate_wave`]; commit needs no re-check.
pub async fn commit_federate_wave(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    prepared: PreparedFederateWave,
) -> Result<CapabilityWaveOutcome, String> {
    let key = ExecuteCoordKey {
        prompt_hash: prompt_hash.to_string(),
        session_id: session_id.to_string(),
    };
    st.session_coordination
        .with_exposure_commit(&key, || async {
            commit_federate_wave_inner(st, prompt_hash, session_id, prepared).await
        })
        .await
}

async fn commit_federate_wave_inner(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    prepared: PreparedFederateWave,
) -> Result<CapabilityWaveOutcome, String> {
    let PreparedFederateWave {
        ctx_arc,
        registry_pin,
        new_entry_id,
        names,
        entry_bindings,
    } = prepared;

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
    let ranked_names = sess.ranked_capabilities.clone();
    let ranked_slice = ranked_names.as_deref();

    if sess.contexts_by_entry.contains_key(&new_entry_id) {
        return Err(format!(
            "session already includes catalog entry `{new_entry_id}`"
        ));
    }

    sess.contexts_by_entry
        .insert(new_entry_id.clone(), ctx_arc.clone());
    sess.registry_catalog_hashes_by_entry
        .insert(new_entry_id.clone(), registry_pin);
    if let Some(b) = entry_bindings {
        sess.bindings_by_entry.insert(new_entry_id.clone(), b);
    }

    let Some(mut exp) = sess.teaching_exposure.take() else {
        return Err("session has no incremental exposure state".into());
    };

    let slots_before = exp.surface.slots.clone();
    let caps_before = exp.surface.capabilities.clone();

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
    let relation_keys = exp.relation_endpoint_keys_for_wave(new_entry_id.as_str(), &names);
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

    let teaching_prompt_chars_added = committed.markdown.chars().count() as u64;
    let reused_session = committed.surface_unchanged && committed.markdown.trim().is_empty();
    Ok(CapabilityWaveOutcome {
        mode: "federate".to_string(),
        entry_id: new_entry_id,
        entities: names,
        markdown_delta: committed.markdown,
        reused_session,
        teaching_prompt_chars_added,
        relations_delta: committed.relations_delta,
    })
}

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
    let prepared = prepare_federate_wave(
        st,
        new_entry_id,
        entities,
        principal,
        outbound_hosted_kv_by_entry,
        bindings_by_entry,
    )
    .await?;
    commit_federate_wave(st, prompt_hash, session_id, prepared).await
}
