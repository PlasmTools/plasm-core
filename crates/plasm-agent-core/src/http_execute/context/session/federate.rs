//! Federate catalog rows.

use super::super::super::*;

use super::super::backend::{
    patch_cgs_context_outbound_hosted, patch_cgs_context_resolved_http_backend,
    resolve_http_backend_for_entry,
};
use super::super::seeds::{
    normalize_execute_entity_names, relation_endpoint_keys_for_wave,
    wrap_teaching_markdown_literal_block,
};

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
    let mut ctx = match reg.load_context(&new_entry_id) {
        Ok(c) => c,
        Err(DiscoveryError::UnknownEntry(id)) => {
            return Err(format!("unknown catalog entry: {id}"));
        }
        Err(e) => return Err(e.to_string()),
    };
    if let Some(map) = outbound_hosted_kv_by_entry {
        if let Some(kv) = map.get(&new_entry_id) {
            ctx = patch_cgs_context_outbound_hosted(ctx, kv);
        }
    }
    let hosted_kv_key = outbound_hosted_kv_by_entry
        .and_then(|map| map.get(&new_entry_id))
        .map(|s| s.as_str());
    let entry_bindings = bindings_by_entry.and_then(|m| m.get(&new_entry_id));
    let catalog_backend =
        crate::http_backend::CatalogHttpBackend::from_cgs_field(ctx.cgs.http_backend.as_str());
    let http_backend = resolve_http_backend_for_entry(
        st,
        new_entry_id.as_str(),
        &catalog_backend,
        entry_bindings,
        hosted_kv_key,
    )
    .await?;
    if catalog_backend.needs_origin_resolution(new_entry_id.as_str()) {
        ctx = patch_cgs_context_resolved_http_backend(ctx, &http_backend);
    }
    let effective_cgs = crate::schema_overlay_session::resolve_schema_overlay_for_host(
        st.engine.as_ref(),
        st.mode,
        st.effective_outbound_secret_provider(),
        ctx.cgs.clone(),
        http_backend.as_str(),
        new_entry_id.as_str(),
    )
    .await?;
    let ctx_arc = Arc::new(plasm_core::CgsContext::entry(
        new_entry_id.clone(),
        effective_cgs,
    ));

    for e in &names {
        if ctx_arc.get_entity(e).is_none() {
            return Err(format!("unknown entity `{e}` in this schema"));
        }
    }

    sess.contexts_by_entry
        .insert(new_entry_id.clone(), ctx_arc.clone());
    if let Some(map) = bindings_by_entry {
        if let Some(b) = map.get(&new_entry_id) {
            sess.bindings_by_entry
                .insert(new_entry_id.clone(), b.clone());
        }
    }

    let Some(mut exp) = sess.teaching_exposure.take() else {
        return Err("session has no incremental exposure state".into());
    };

    let layers: Vec<&CGS> = sess
        .contexts_by_entry
        .values()
        .map(|c| c.cgs.as_ref())
        .collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let n0 = exp.entities.len();
    if let Some(ref intent_s) = scope_intent {
        let relation_keys = relation_endpoint_keys_for_wave(&exp, new_entry_id.as_str(), &names);
        let delta = plasm_core::discovery::derive_intent_exposure_surface_batch(
            ctx_arc.cgs.as_ref(),
            new_entry_id.as_str(),
            intent_s.as_str(),
            &relation_keys,
            &names,
            ranked_slice,
            plasm_core::discovery::ExposureSurfaceOptions {
                read_first_seeded: true,
            },
        );
        exp.expose_surface(
            &layers,
            ctx_arc.cgs.clone(),
            new_entry_id.as_str(),
            &refs,
            delta,
        );
    } else {
        exp.expose_entities(&layers, ctx_arc.cgs.clone(), new_entry_id.as_str(), &refs);
    }
    let added_qualified = exp.qualified_entities_since(n0);

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
        .render_teaching_exposure_delta_federated(
            &by_entry,
            &exp,
            &added_qualified,
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
    })
}
