//! Execute session open, federate, expand, and capability seed application.

use super::super::*;

use super::backend::{
    patch_cgs_context_outbound_hosted, patch_cgs_context_resolved_http_backend,
    resolve_http_backend_for_entry, tenant_outbound_hosted_kv_for_entries,
};
use super::seeds::{
    apply_ranked_capabilities_session_update, build_capability_exposure_plan,
    format_session_unchanged_one_liner, normalize_execute_entity_names,
    normalize_ranked_capabilities_for_gate, process_order_for_expand_group,
    relation_endpoint_keys_for_wave, seeds_fully_exposed, wrap_teaching_markdown_literal_block,
    RankedCapabilitiesArg, STALE_EXECUTE_BINDING_NOTICE,
};
async fn execute_session_create_response_inner(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    body: CreateExecuteSessionBody,
    allow_reuse: bool,
    outbound_hosted_kv_by_entry: Option<&HashMap<String, String>>,
    bindings_by_entry: Option<&HashMap<String, crate::binding_slots::SessionBindingMap>>,
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

    let names = normalize_execute_entity_names(body.entities);

    let reg = st.catalog.snapshot();
    let mut ctx = match reg.load_context(&body.entry_id) {
        Ok(c) => c,
        Err(DiscoveryError::UnknownEntry(id)) => {
            crate::metrics::record_execute_session_outcome("error", "unknown_entry");
            return Err(format!("unknown catalog entry: {id}"));
        }
        Err(e) => {
            crate::metrics::record_execute_session_outcome("error", "discovery");
            return Err(e.to_string());
        }
    };
    if let Some(map) = outbound_hosted_kv_by_entry {
        if let Some(kv) = map.get(&body.entry_id) {
            ctx = patch_cgs_context_outbound_hosted(ctx, kv);
        }
    }
    let hosted_kv_key = outbound_hosted_kv_by_entry
        .and_then(|map| map.get(&body.entry_id))
        .map(|s| s.as_str());
    let entry_bindings = bindings_by_entry.and_then(|m| m.get(&body.entry_id));
    let catalog_backend =
        crate::http_backend::CatalogHttpBackend::from_cgs_field(ctx.cgs.http_backend.as_str());
    let http_backend = resolve_http_backend_for_entry(
        st,
        body.entry_id.as_str(),
        &catalog_backend,
        entry_bindings,
        hosted_kv_key,
    )
    .await?;
    if catalog_backend.needs_origin_resolution(body.entry_id.as_str()) {
        ctx = patch_cgs_context_resolved_http_backend(ctx, &http_backend);
    }
    let ctx_arc = Arc::new(ctx);
    let effective_cgs = crate::schema_overlay_session::resolve_schema_overlay_for_host(
        st.engine.as_ref(),
        st.mode,
        st.effective_outbound_secret_provider(),
        ctx_arc.cgs.clone(),
        http_backend.as_str(),
        body.entry_id.as_str(),
    )
    .await?;
    let catalog_cgs_hash = effective_cgs.effective_catalog_cgs_hash_hex();
    let ctx_arc = Arc::new(plasm_core::CgsContext::entry(
        body.entry_id.clone(),
        effective_cgs.clone(),
    ));

    let plugin_generation = st
        .plugin_manager
        .as_ref()
        .and_then(|m| m.current_generation());
    let plugin_generation_id = plugin_generation.as_ref().map(|g| g.id);

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
        entities: names.clone(),
        context_intent: domain_filter_intent.clone(),
        ranked_capabilities: ranked_for_domain.clone(),
        principal: principal_stored.clone(),
        plugin_generation_id,
        logical_session_id: body.logical_session_id.map(|u| u.hyphenated().to_string()),
    };

    if allow_reuse {
        if let Some((session_id_str, sess)) = st.sessions.try_reuse_session(&reuse_key).await {
            let _reuse = crate::spans::execute_session_reuse(
                reuse_key.entry_id.as_str(),
                reuse_key.catalog_cgs_hash.as_str(),
                sess.prompt_hash.as_str(),
                session_id_str.as_str(),
            )
            .entered();
            tracing::info!(
                entry_id = %reuse_key.entry_id,
                entities = ?reuse_key.entities,
                catalog_cgs_hash = %reuse_key.catalog_cgs_hash,
                prompt_hash = %sess.prompt_hash,
                session = %session_id_str,
                "reusing execute session (same entry_id + entities + catalog hash)"
            );
            crate::metrics::record_execute_session_outcome("reuse", "");
            return Ok(create_execute_session_response(
                &sess,
                session_id_str,
                sess.prompt_text.clone(),
                true,
            ));
        }
    }

    let mut contexts_by_entry = IndexMap::new();
    contexts_by_entry.insert(body.entry_id.clone(), ctx_arc.clone());

    let cgs: Arc<CGS> = effective_cgs;
    for e in &names {
        if cgs.get_entity(e).is_none() {
            crate::metrics::record_execute_session_outcome("error", "unknown_entity");
            return Err(format!("unknown entity `{e}` in this schema"));
        }
    }

    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let teaching_exposure = match &domain_filter_intent {
        Some(intent_s) => {
            let relation_keys = plasm_core::relation_endpoint_keys(body.entry_id.as_str(), &names);
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
    let sym_cross = st.sessions.symbol_map_cross_cache();
    let teaching_prompt = st
        .engine
        .prompt_pipeline()
        .render_teaching_first_wave_for_session(cgs.as_ref(), &teaching_exposure, Some(sym_cross));
    let prompt = wrap_teaching_markdown_literal_block(
        &teaching_prompt,
        st.engine.prompt_pipeline().render_mode,
    );
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

    let session = ExecuteSession::new_with_bindings(
        prompt_hash_str.clone(),
        prompt.clone(),
        cgs,
        contexts_by_entry,
        body.entry_id.clone(),
        scope,
        subj,
        Some(http_backend.as_str().to_string()),
        names.clone(),
        Some(teaching_exposure),
        principal_stored.clone(),
        plugin_generation,
        catalog_cgs_hash,
        domain_filter_intent,
        ranked_for_domain,
        bindings_map,
    );
    st.store_execute_session(
        reuse_key,
        prompt_hash_str.clone(),
        session_id_str.clone(),
        session,
    )
    .instrument(create_span)
    .await;

    crate::metrics::record_execute_session_outcome("create", "");
    Ok(CreateExecuteSessionResponse {
        prompt_hash: prompt_hash_str,
        session: session_id_str,
        prompt,
        entry_id: body.entry_id,
        entities: names,
        grammar_revision: plasm_grammar_frontmatter_revision_hex().to_string(),
        reused: false,
        principal: principal_stored,
    })
}

pub async fn execute_session_create_response(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    body: CreateExecuteSessionBody,
) -> Result<CreateExecuteSessionResponse, String> {
    execute_session_create_response_inner(st, principal, body, true, None, None).await
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

/// Append expand-wave Plasm instruction blocks for more entity names; [`TeachingExposureSession`] keeps `e#`/`m#`/`p#` stable.
pub async fn expand_execute_teaching_session(
    st: &PlasmHostState,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    prompt_hash: &str,
    session_id: &str,
    seeds: Vec<CapabilitySeed>,
) -> Result<String, String> {
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

    if added_qualified.is_empty() {
        sess.entities = exp.entities.clone();
        sess.teaching_exposure = Some(exp);
        st.replace_execute_session(prompt_hash_p.as_str(), session_id_p.as_str(), sess)
            .await;
        return Ok(String::new());
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
            .render_teaching_exposure_delta_federated(
                &by_entry,
                &exp,
                &added_qualified,
                Some(sym_cross),
            )
    } else {
        st.engine.prompt_pipeline().render_teaching_exposure_delta(
            cgs_primary,
            &exp,
            &added,
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
    Ok(wave)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_capability_seeds(
    st: &PlasmHostState,
    principal_incoming: Option<&crate::incoming_auth::TenantPrincipal>,
    binding: Option<(&str, &str)>,
    seeds: Vec<CapabilitySeed>,
    principal: Option<String>,
    tenant_mcp_cfg: Option<Arc<crate::mcp_runtime_config::McpRuntimeConfig>>,
    logical_session_id: Option<Uuid>,
    plasm_context_intent: &str,
    ranked_capabilities: RankedCapabilitiesArg,
) -> Result<ApplyCapabilitySeedsOutcome, String> {
    let seeds = normalize_capability_seeds(seeds);
    if seeds.is_empty() {
        return Err("`seeds` must be non-empty".into());
    }
    let seeds = resolve_capability_seeds(seeds, &st.catalog.snapshot(), None)?;

    // MCP `PlasmExecBinding` can outlive the in-memory [`ExecuteSessionStore`] row (idle expiry).
    // Treat a binding as absent so we open a fresh execute session instead of failing federate/expand.
    let mut stale_execute_binding_recovered = false;
    let mut stale_binding_previous: Option<(String, String)> = None;
    let had_binding = binding.is_some();
    let binding = match binding {
        None => None,
        Some((ph, sid)) => {
            if st.get_execute_session(ph, sid).await.is_some() {
                Some((ph, sid))
            } else {
                stale_execute_binding_recovered = true;
                stale_binding_previous = Some((ph.to_string(), sid.to_string()));
                tracing::info!(
                    target: "plasm_agent::http_execute",
                    prompt_hash = %ph,
                    session_id = %sid,
                    "apply_capability_seeds: MCP execute binding stale (session missing or expired); opening fresh execute session"
                );
                None
            }
        }
    };

    let plan = build_capability_exposure_plan(&seeds)
        .ok_or_else(|| "internal error: empty capability exposure plan".to_string())?;
    let primary_entry_id = plan.primary_entry_id.clone();

    let mut all_eids: Vec<String> = plan.seeds_by_entry.keys().cloned().collect();
    all_eids.sort();
    let outbound_map_storage = if let Some(ref cfg) = tenant_mcp_cfg {
        Some(
            tenant_outbound_hosted_kv_for_entries(st, cfg.as_ref(), principal_incoming, &all_eids)
                .await,
        )
    } else {
        None
    };
    let outbound_ref = outbound_map_storage.as_ref();
    let bindings_map_storage = if let Some(ref cfg) = tenant_mcp_cfg {
        Some(
            crate::session_bindings::tenant_bindings_for_entries(st, cfg.as_ref(), &all_eids)
                .await?,
        )
    } else if let Some(engine_base) = st.engine.config().base_url.as_deref() {
        let override_url = crate::http_backend::ReplHttpOverride::from_engine_base(engine_base)
            .map_err(|e| format!("invalid engine base_url: {e}"))?;
        let mut map = HashMap::new();
        for eid in &all_eids {
            if let Some(m) = crate::session_bindings::repl_session_binding_map(
                eid.as_str(),
                override_url.clone(),
            ) {
                map.insert(eid.clone(), m);
            }
        }
        if map.is_empty() {
            None
        } else {
            Some(map)
        }
    } else {
        None
    };
    let bindings_ref = bindings_map_storage.as_ref();

    let mut waves = Vec::new();
    let mut new_symbol_space = false;
    let (prompt_hash, session_id, binding_updated) = match binding {
        None => {
            let primary_entities = plan
                .seeds_by_entry
                .get(&primary_entry_id)
                .cloned()
                .ok_or_else(|| "missing primary entities".to_string())?;
            let created = execute_session_create_response_inner(
                st,
                principal_incoming,
                CreateExecuteSessionBody {
                    entry_id: primary_entry_id.clone(),
                    entities: primary_entities.clone(),
                    principal: principal.clone(),
                    logical_session_id,
                    context_intent: normalize_context_intent_for_domain_filter(Some(
                        plasm_context_intent,
                    )),
                    ranked_capabilities: match &ranked_capabilities {
                        RankedCapabilitiesArg::Unspecified => None,
                        RankedCapabilitiesArg::Set(opt) => opt.clone(),
                    },
                    read_first_seeded_exposure: true,
                },
                plan.seeds_by_entry.len() <= 1,
                outbound_ref,
                bindings_ref,
            )
            .await?;
            new_symbol_space = !created.reused;
            let mut open_md = String::new();
            if stale_execute_binding_recovered {
                open_md.push_str(STALE_EXECUTE_BINDING_NOTICE);
            }
            if created.reused {
                open_md.push_str(&format_session_unchanged_one_liner(
                    created.entities.len().max(1),
                ));
            } else {
                let mode = st.engine.prompt_pipeline().render_mode;
                if mode.is_tsv() {
                    if let Some(body_tsv) = teaching_tsv_table_from_wrapped_prompt(
                        &created.prompt,
                        mode.markdown_fence_info_string(),
                    ) {
                        open_md.push_str(&wrap_teaching_markdown_literal_block(&body_tsv, mode));
                    } else {
                        open_md.push_str(&created.prompt);
                    }
                } else {
                    open_md.push_str(&created.prompt);
                }
            }
            let teaching_prompt_chars_added = if created.reused {
                0
            } else {
                open_md.chars().count() as u64
            };
            waves.push(CapabilityWaveOutcome {
                mode: "open".to_string(),
                entry_id: created.entry_id.clone(),
                entities: primary_entities,
                markdown_delta: open_md.clone(),
                reused_session: created.reused,
                teaching_prompt_chars_added,
            });
            (created.prompt_hash, created.session, true)
        }
        Some((ph, sid)) => (ph.to_string(), sid.to_string(), false),
    };

    if had_binding {
        apply_ranked_capabilities_session_update(
            st,
            prompt_hash.as_str(),
            session_id.as_str(),
            &ranked_capabilities,
        )
        .await?;
        if let Some(sess_arc) = st.get_execute_session(&prompt_hash, &session_id).await {
            if let Some(ref exp) = sess_arc.teaching_exposure {
                let catalogs_ready = plan
                    .process_order
                    .iter()
                    .all(|eid| sess_arc.contexts_by_entry.contains_key(eid));
                if catalogs_ready && seeds_fully_exposed(exp, &seeds) {
                    let n = exp.entities.len().max(1);
                    return Ok(ApplyCapabilitySeedsOutcome {
                        prompt_hash,
                        session_id,
                        primary_entry_id: primary_entry_id.clone(),
                        principal,
                        waves: vec![CapabilityWaveOutcome {
                            mode: "expand".to_string(),
                            entry_id: primary_entry_id,
                            entities: vec![],
                            markdown_delta: format_session_unchanged_one_liner(n),
                            reused_session: true,
                            teaching_prompt_chars_added: 0,
                        }],
                        binding_updated,
                        new_symbol_space: false,
                        stale_execute_binding_recovered,
                        stale_binding_previous,
                    });
                }
            }
        }
    }

    for eid in &plan.process_order {
        if *eid == primary_entry_id && binding.is_none() {
            continue;
        }
        let Some(entities) = plan.seeds_by_entry.get(eid) else {
            continue;
        };
        let has_session_entry = st
            .sessions
            .get_by_strs(&prompt_hash, &session_id)
            .await
            .map(|s| s.contexts_by_entry.contains_key(eid))
            .unwrap_or(false);
        if !has_session_entry {
            let wave = federate_execute_session(
                st,
                prompt_hash.as_str(),
                session_id.as_str(),
                eid.clone(),
                entities.clone(),
                principal.clone(),
                outbound_ref,
                bindings_ref,
            )
            .await?;
            waves.push(wave);
        } else {
            let md = expand_execute_teaching_session(
                st,
                principal_incoming,
                prompt_hash.as_str(),
                session_id.as_str(),
                entities
                    .iter()
                    .map(|e| CapabilitySeed {
                        entry_id: eid.clone(),
                        entity: e.clone(),
                    })
                    .collect(),
            )
            .await?;
            waves.push(CapabilityWaveOutcome {
                mode: "expand".to_string(),
                entry_id: eid.clone(),
                entities: entities.clone(),
                teaching_prompt_chars_added: md.chars().count() as u64,
                markdown_delta: md,
                reused_session: false,
            });
        }
    }

    if waves.len() > 1
        && waves
            .iter()
            .all(|w| w.teaching_prompt_chars_added == 0 && w.markdown_delta.trim().is_empty())
    {
        let n = st
            .sessions
            .get_by_strs(&prompt_hash, &session_id)
            .await
            .and_then(|s| s.teaching_exposure.as_ref().map(|e| e.entities.len()))
            .unwrap_or(0)
            .max(1);
        waves = vec![CapabilityWaveOutcome {
            mode: "expand".to_string(),
            entry_id: primary_entry_id.clone(),
            entities: vec![],
            markdown_delta: format_session_unchanged_one_liner(n),
            reused_session: true,
            teaching_prompt_chars_added: 0,
        }];
    }

    Ok(ApplyCapabilitySeedsOutcome {
        prompt_hash,
        session_id,
        primary_entry_id,
        principal,
        waves,
        binding_updated,
        new_symbol_space,
        stale_execute_binding_recovered,
        stale_binding_previous,
    })
}
