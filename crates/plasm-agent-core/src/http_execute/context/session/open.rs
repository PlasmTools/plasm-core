//! Open execute sessions.

use super::super::super::*;

use super::super::backend::tenant_outbound_hosted_kv_for_entries;
use super::super::seeds::{
    apply_ranked_capabilities_session_update, build_capability_exposure_plan,
    dedup_preserve_arrival_order, normalize_ranked_capabilities_for_gate,
    sorted_entity_set_for_reuse_key, process_order_for_expand_group, relation_endpoint_keys_for_wave,
    seeds_fully_exposed, wrap_teaching_markdown_literal_block, RankedCapabilitiesArg,
    STALE_EXECUTE_BINDING_NOTICE,
};
use super::symbol_ledger::persist_from_execute_row;

pub(crate) async fn execute_session_create_response_inner(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    body: CreateExecuteSessionBody,
    allow_reuse: bool,
    outbound_hosted_kv_by_entry: Option<&HashMap<String, String>>,
    bindings_by_entry: Option<&HashMap<String, crate::binding_slots::SessionBindingMap>>,
    restored_teaching_exposure: Option<plasm_core::TeachingExposureSession>,
    symbol_space_reset: bool,
) -> Result<CreateExecuteSessionResponse, String> {
    if body.entities.is_empty() {
        crate::metrics::record_execute_session_outcome("error", "empty_entities");
        return Err("`entities` must be non-empty".into());
    }

    let mode = auth_resolution_mode_from_env();
    validate_principal_for_mode(mode, body.principal.as_deref()).inspect_err(|_| {
        crate::metrics::record_execute_session_outcome("error", "principal_validation");
    })?;
    let principal_stored: Option<String> = match mode {
        AuthResolutionMode::Env => None,
        AuthResolutionMode::Delegated => body.principal.as_ref().map(|s| s.trim().to_string()),
    };

    let names = dedup_preserve_arrival_order(body.entities);
    let reuse_key_entities = sorted_entity_set_for_reuse_key(&names);

    let reg = st.catalog.snapshot();
    let registry_catalog_hashes =
        crate::execute_session_rehydrate::registry_catalog_pins_from_registry(
            reg.as_ref(),
            std::slice::from_ref(&body.entry_id),
        )
        .map_err(|e| e.to_string())?
        .registry_hash_by_entry;

    let hosted_kv_key = outbound_hosted_kv_by_entry
        .and_then(|map| map.get(&body.entry_id))
        .map(|s| s.as_str());
    let entry_bindings = bindings_by_entry.and_then(|m| m.get(&body.entry_id));
    let materialized = crate::execute_session_materialize::materialize_entry_context(
        st,
        body.entry_id.as_str(),
        hosted_kv_key,
        entry_bindings,
    )
    .await?;
    let ctx_arc = materialized.ctx;
    let effective_cgs = materialized.effective_cgs;
    let http_backend = materialized.http_backend;
    let catalog_cgs_hash = effective_cgs.effective_catalog_cgs_hash_hex();

    let scope = tenant_scope(principal);
    let subj = principal.map(|p| p.subject.clone()).unwrap_or_default();

    let domain_filter_intent =
        normalize_context_intent_for_domain_filter(body.context_intent.as_deref());

    let ranked_for_domain = domain_filter_intent
        .as_ref()
        .and_then(|_| normalize_ranked_capabilities_for_gate(body.ranked_capabilities.clone()));

    let reuse_key = SessionReuseKey {
        tenant_scope: scope.clone(),
        entry_id: body.entry_id.clone(),
        catalog_cgs_hash: catalog_cgs_hash.clone(),
        entities: reuse_key_entities.clone(),
        context_intent: domain_filter_intent.clone(),
        ranked_capabilities: ranked_for_domain.clone(),
        principal: principal_stored.clone(),
        logical_session_id: body.logical_session_id.map(|u| u.hyphenated().to_string()),
    };

    if allow_reuse {
        if let Some((session_id_str, sess)) = st.sessions.try_reuse_session(&reuse_key).await {
            if let Some(reused) = st
                .get_execute_session(sess.prompt_hash.as_str(), session_id_str.as_str())
                .await
            {
                let _reuse = crate::spans::execute_session_reuse(
                    reuse_key.entry_id.as_str(),
                    reuse_key.catalog_cgs_hash.as_str(),
                    reused.prompt_hash.as_str(),
                    session_id_str.as_str(),
                )
                .entered();
                tracing::info!(
                    entry_id = %reuse_key.entry_id,
                    entities = ?reuse_key.entities,
                    catalog_cgs_hash = %reuse_key.catalog_cgs_hash,
                    prompt_hash = %reused.prompt_hash,
                    session = %session_id_str,
                    "reusing execute session (same entry_id + entities + catalog hash)"
                );
                crate::metrics::record_execute_session_outcome("reuse", "");
                return Ok(create_execute_session_response(
                    &reused,
                    session_id_str,
                    reused.prompt_text.clone(),
                    true,
                ));
            }
        }
    }

    let mut contexts_by_entry = IndexMap::new();
    contexts_by_entry.insert(body.entry_id.clone(), ctx_arc.clone());

    let cgs: Arc<CGS> = effective_cgs;
    let (session_entity_names, teaching_exposure) = if let Some(restored) = restored_teaching_exposure {
        for e in &restored.entities {
            if cgs.get_entity(e).is_none() {
                crate::metrics::record_execute_session_outcome("error", "unknown_entity");
                return Err(format!("unknown entity `{e}` in restored symbol ledger"));
            }
        }
        (restored.entities.clone(), restored)
    } else {
        for e in &names {
            if cgs.get_entity(e).is_none() {
                crate::metrics::record_execute_session_outcome("error", "unknown_entity");
                return Err(format!("unknown entity `{e}` in this schema"));
            }
        }
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let built = match &domain_filter_intent {
            Some(intent_s) => {
                let relation_keys =
                    plasm_core::relation_endpoint_keys(body.entry_id.as_str(), &names);
                let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
                    cgs.as_ref(),
                    body.entry_id.as_str(),
                    intent_s.as_str(),
                    &relation_keys,
                    &names,
                    ranked_for_domain.as_deref(),
                    plasm_core::discovery::ExposureSurfaceOptions {
                        read_first_seeded: body.read_first_seeded_exposure,
                    },
                );
                plasm_core::TeachingExposureSession::new_with_intent_delta(
                    cgs.as_ref(),
                    body.entry_id.as_str(),
                    &refs,
                    delta,
                )
            }
            None => {
                plasm_core::TeachingExposureSession::new(cgs.as_ref(), body.entry_id.as_str(), &refs)
            }
        };
        (names.clone(), built)
    };
    let sym_cross = st.sessions.symbol_map_cross_cache();
    let teaching_prompt = st
        .engine
        .prompt_pipeline()
        .render_teaching_first_wave_for_session(cgs.as_ref(), &teaching_exposure, Some(sym_cross));
    let mut prompt = wrap_teaching_markdown_literal_block(
        &teaching_prompt,
        st.engine.prompt_pipeline().render_mode,
    );
    if symbol_space_reset {
        prompt = format!(
            "{}{}",
            super::super::seeds::SYMBOL_SPACE_RESET_NOTICE,
            prompt
        );
    }
    let prompt_hash = PromptHashHex::from_prompt_sha256(&prompt);
    let session_id = ExecuteSessionId::new_random();
    let prompt_hash_str = prompt_hash.to_string();
    let session_id_str = session_id.to_string();

    let create_span = crate::spans::execute_session_create();
    tracing::debug!(
        tenant_scope = %scope,
        principal = %subj,
        entry_id = %body.entry_id,
        "execute session created"
    );

    let mut bindings_map = indexmap::IndexMap::new();
    if let Some(map) = bindings_by_entry {
        if let Some(b) = map.get(&body.entry_id) {
            bindings_map.insert(body.entry_id.clone(), b.clone());
        }
    }

    let mut session = ExecuteSession::new_with_bindings(
        prompt_hash_str.clone(),
        prompt.clone(),
        cgs,
        contexts_by_entry,
        body.entry_id.clone(),
        scope,
        subj,
        Some(http_backend.as_str().to_string()),
        session_entity_names.clone(),
        Some(teaching_exposure),
        principal_stored.clone(),
        catalog_cgs_hash,
        domain_filter_intent,
        ranked_for_domain,
        bindings_map,
    );
    session.registry_catalog_hashes_by_entry = registry_catalog_hashes;
    st.store_execute_session(
        reuse_key,
        prompt_hash_str.clone(),
        session_id_str.clone(),
        session,
    )
    .instrument(create_span)
    .await;

    if let Some(uuid) = body.logical_session_id {
        persist_from_execute_row(
            st,
            Some(uuid),
            prompt_hash_str.as_str(),
            session_id_str.as_str(),
        )
        .await;
    }

    crate::metrics::record_execute_session_outcome("create", "");
    Ok(CreateExecuteSessionResponse {
        prompt_hash: prompt_hash_str,
        session: session_id_str,
        prompt,
        entry_id: body.entry_id,
        entities: session_entity_names,
        reused: false,
        principal: principal_stored,
    })
}

pub async fn execute_session_create_response(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    body: CreateExecuteSessionBody,
) -> Result<CreateExecuteSessionResponse, String> {
    execute_session_create_response_inner(st, principal, body, true, None, None, None, false).await
}
