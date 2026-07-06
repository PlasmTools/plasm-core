//! Apply capability seeds.

use super::super::super::*;

use super::super::backend::tenant_outbound_hosted_kv_for_entries;
use super::super::seeds::{
    apply_context_intent_session_update, apply_ranked_capabilities_session_update,
    build_capability_exposure_plan, format_session_unchanged_reuse_markdown,
    group_seed_entities_by_entry, normalize_ranked_capabilities_for_gate,
    primary_entry_id_for_grouped, ranked_capabilities_need_exposure_replay,
    seeds_exposure_ready_for_reuse, teaching_exposure_at, unchanged_expand_wave,
    wrap_teaching_markdown_literal_block, RankedCapabilitiesArg, STALE_EXECUTE_BINDING_NOTICE,
};
use super::expand::expand_execute_teaching_session;
use super::federate::{commit_federate_wave, prepare_federate_wave, PreparedFederateWave};
use super::open::execute_session_create_response_inner;
use super::symbol_ledger::{persist_from_execute_row, resolve_restore_for_open};
use crate::session_coordination::ExecuteCoordKey;

/// Live execute binding resolved for a `plasm_context` / seeds application.
struct ResolvedExecuteBinding {
    /// `(prompt_hash, session_id)` to reuse, when a valid session exists (explicit or logical-hydrated).
    binding: Option<(String, String)>,
    /// The caller passed an explicit binding (even if it turned out stale).
    had_binding: bool,
    /// The binding was re-hydrated from `logical_execute_bindings` (caller passed none or a stale one).
    hydrated: bool,
    /// The caller's binding pointed at a missing/expired session; a fresh session will be opened.
    stale_execute_binding_recovered: bool,
    /// The stale `(prompt_hash, session_id)` recovered from, surfaced in caller notices.
    stale_binding_previous: Option<(String, String)>,
}

/// Resolve the execute binding to reuse. An MCP `PlasmExecBinding` can outlive the in-memory
/// [`ExecuteSessionStore`] row (idle expiry / catalog reload), so a missing session is treated as
/// absent (open fresh) instead of failing federate/expand; a `logical_session_id` may also
/// re-hydrate a live binding. Gate-free: only async session lookups, no coordination locks.
async fn resolve_execute_binding(
    st: &PlasmHostState,
    binding: Option<(&str, &str)>,
    logical_session_id: Option<Uuid>,
) -> ResolvedExecuteBinding {
    let had_binding = binding.is_some();
    let mut stale_execute_binding_recovered = false;
    let mut stale_binding_previous: Option<(String, String)> = None;
    let mut resolved: Option<(String, String)> = match binding {
        None => None,
        Some((ph, sid)) => {
            if st.get_execute_session(ph, sid).await.is_some() {
                Some((ph.to_string(), sid.to_string()))
            } else {
                stale_execute_binding_recovered = true;
                stale_binding_previous = Some((ph.to_string(), sid.to_string()));
                tracing::info!(
                    target: "plasm_agent::http_execute",
                    prompt_hash = %ph,
                    session_id = %sid,
                    "apply_capability_seeds: MCP execute binding stale (session missing, expired, or catalog reload); opening fresh execute session"
                );
                None
            }
        }
    };

    let mut hydrated = false;
    if resolved.is_none() {
        if let Some(uuid) = logical_session_id {
            if let Some(pair) = st.logical_execute_bindings.get(&uuid).await {
                if st.get_execute_session(&pair.0, &pair.1).await.is_some() {
                    resolved = Some(pair);
                    hydrated = true;
                }
            }
        }
    }

    ResolvedExecuteBinding {
        binding: resolved,
        had_binding,
        hydrated,
        stale_execute_binding_recovered,
        stale_binding_previous,
    }
}

/// outcomes to append. Two passes preserve the lock-light design: all federate network I/O
/// ([`prepare_federate_wave`]) runs first (no coordination gate held), then per-row commits
/// ([`commit_federate_wave`]) and `expand` waves apply under their per-execute-row exposure-commit
/// gates (CEP-13). `skip_primary_open` drops the primary entry when it was just opened in this call.
#[allow(clippy::too_many_arguments)]
async fn commit_federate_and_expand_waves(
    st: &PlasmHostState,
    principal_incoming: Option<&crate::incoming_auth::TenantPrincipal>,
    prompt_hash: &str,
    session_id: &str,
    plan: &super::super::seeds::CapabilityExposurePlan,
    skip_primary_open: bool,
    principal: Option<String>,
    outbound_ref: Option<&HashMap<String, String>>,
    bindings_ref: Option<&HashMap<String, crate::binding_slots::SessionBindingMap>>,
) -> Result<Vec<CapabilityWaveOutcome>, super::SessionMutateError> {
    let mut prepared_federates: HashMap<String, PreparedFederateWave> = HashMap::new();
    for eid in &plan.process_order {
        if skip_primary_open && *eid == plan.primary_entry_id {
            continue;
        }
        let Some(entities) = plan.seeds_by_entry.get(eid) else {
            continue;
        };
        let has_session_entry = st
            .sessions
            .get_by_strs(prompt_hash, session_id)
            .await
            .map(|s| s.contexts_by_entry.contains_key(eid))
            .unwrap_or(false);
        if !has_session_entry {
            let prepared = prepare_federate_wave(
                st,
                eid.clone(),
                entities.clone(),
                principal.clone(),
                outbound_ref,
                bindings_ref,
            )
            .await
            .map_err(super::SessionMutateError::from)?;
            prepared_federates.insert(eid.clone(), prepared);
        }
    }

    let mut waves = Vec::new();
    for eid in &plan.process_order {
        if skip_primary_open && *eid == plan.primary_entry_id {
            continue;
        }
        let Some(entities) = plan.seeds_by_entry.get(eid) else {
            continue;
        };
        if let Some(prepared) = prepared_federates.remove(eid) {
            let wave = commit_federate_wave(st, prompt_hash, session_id, prepared).await?;
            waves.push(wave);
        } else {
            let expand = expand_execute_teaching_session(
                st,
                principal_incoming,
                prompt_hash,
                session_id,
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
                teaching_prompt_chars_added: expand.markdown.chars().count() as u64,
                markdown_delta: expand.markdown,
                reused_session: false,
                relations_delta: expand.relations_delta,
            });
        }
    }
    Ok(waves)
}

#[allow(clippy::too_many_arguments)]
pub async fn apply_capability_seeds(
    st: &PlasmHostState,
    principal_incoming: Option<&crate::incoming_auth::TenantPrincipal>,
    binding: Option<(&str, &str)>,
    seeds: Vec<CapabilitySeed>,
    principal: Option<String>,
    tenant_mcp_cfg: Option<Arc<crate::mcp_runtime_config::McpRuntimeConfig>>,
    logical_session_id: Option<Uuid>,
    plasm_context_intent: &str,
    ranked_capabilities: RankedCapabilitiesArg,
) -> Result<ApplyCapabilitySeedsOutcome, super::SessionMutateError> {
    let mut seeds = normalize_capability_seeds(seeds);

    let ResolvedExecuteBinding {
        binding,
        had_binding,
        hydrated,
        stale_execute_binding_recovered,
        stale_binding_previous,
    } = resolve_execute_binding(st, binding, logical_session_id).await;

    if seeds.is_empty() {
        if matches!(
            &ranked_capabilities,
            RankedCapabilitiesArg::Set(Some(list)) if !list.is_empty()
        ) {
            if let Some((ph, sid)) = &binding {
                if let Some(sess_arc) = st.get_execute_session(ph, sid).await {
                    seeds = super::super::seeds::capability_seeds_from_session(sess_arc.as_ref());
                }
            }
        }
        if seeds.is_empty() {
            return Err(
                "`seeds` must be non-empty when opening a new symbol space. To surface write \
                 capabilities via `ranked_capabilities`, reuse the same logical session binding \
                 from a prior `plasm_context` call (same seeds) or pass `seeds` again."
                    .into(),
            );
        }
    }
    let seeds = resolve_capability_seeds(seeds, &st.catalog.snapshot(), None)?;

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

    let flow_policy_scope: Option<(&str, &str, &str)> = tenant_mcp_cfg.as_ref().and_then(|cfg| {
        let ws = cfg.workspace_slug.trim();
        let ps = cfg.project_slug.trim();
        if ws.is_empty() || ps.is_empty() {
            None
        } else {
            Some((cfg.tenant_id.as_str(), ws, ps))
        }
    });

    let mut waves = Vec::new();
    let mut new_symbol_space = false;
    let mut symbol_space_reset = false;
    let (prompt_hash, session_id, binding_updated) = if let Some((ph, sid)) = &binding {
        (ph.clone(), sid.clone(), false)
    } else {
        let open_body = CreateExecuteSessionBody {
            entry_id: primary_entry_id.clone(),
            entities: plan
                .seeds_by_entry
                .get(&primary_entry_id)
                .cloned()
                .ok_or_else(|| "missing primary entities".to_string())?,
            principal: principal.clone(),
            logical_session_id,
            context_intent: normalize_context_intent_for_domain_filter(Some(plasm_context_intent)),
            ranked_capabilities: match &ranked_capabilities {
                RankedCapabilitiesArg::Unspecified => None,
                RankedCapabilitiesArg::Set(opt) => opt.clone(),
            },
            read_first_seeded_exposure: true,
        };
        let primary_entities = open_body.entities.clone();
        let (restored_exposure, ledger_reset) =
            resolve_restore_for_open(st, logical_session_id, outbound_ref, bindings_ref).await;
        symbol_space_reset = ledger_reset;
        let had_restored_ledger = restored_exposure.is_some();

        let created = if let Some(uuid) = logical_session_id {
            st.session_coordination
                .with_logical_open(uuid, || async {
                    if let Some(pair) = st.logical_execute_bindings.get(&uuid).await {
                        if let Some(sess_arc) = st.get_execute_session(&pair.0, &pair.1).await {
                            return Ok::<CreateExecuteSessionResponse, super::SessionMutateError>(
                                CreateExecuteSessionResponse {
                                    prompt_hash: pair.0,
                                    session: pair.1,
                                    entry_id: sess_arc.entry_id.clone(),
                                    prompt: sess_arc.prompt_text.clone(),
                                    entities: sess_arc.entities.clone(),
                                    reused: true,
                                    principal: sess_arc.principal.clone(),
                                },
                            );
                        }
                    }
                    let created = execute_session_create_response_inner(
                        st,
                        principal_incoming,
                        open_body,
                        plan.seeds_by_entry.len() <= 1,
                        outbound_ref,
                        bindings_ref,
                        restored_exposure,
                        symbol_space_reset,
                        flow_policy_scope,
                    )
                    .await?;
                    st.logical_execute_bindings
                        .insert(uuid, created.prompt_hash.clone(), created.session.clone())
                        .await;
                    persist_from_execute_row(
                        st,
                        Some(uuid),
                        created.prompt_hash.as_str(),
                        created.session.as_str(),
                    )
                    .await;
                    Ok(created)
                })
                .await?
        } else {
            execute_session_create_response_inner(
                st,
                principal_incoming,
                open_body,
                plan.seeds_by_entry.len() <= 1,
                outbound_ref,
                bindings_ref,
                restored_exposure,
                symbol_space_reset,
                flow_policy_scope,
            )
            .await?
        };

        new_symbol_space = !created.reused && !had_restored_ledger;
        if symbol_space_reset {
            new_symbol_space = true;
        }
        let mut open_md = String::new();
        if stale_execute_binding_recovered {
            open_md.push_str(STALE_EXECUTE_BINDING_NOTICE);
        }
        if created.reused {
            let exposure =
                teaching_exposure_at(st, created.prompt_hash.as_str(), created.session.as_str())
                    .await;
            open_md.push_str(&format_session_unchanged_reuse_markdown(exposure.as_ref()));
        } else {
            let mode = st.engine.prompt_pipeline().render_mode;
            if mode.is_tsv() {
                if let Some(body_tsv) = teaching_tsv_from_wrapped_prompt(
                    &created.prompt,
                    mode.markdown_fence_info_string(),
                    TeachingFenceSlice::TableOnly,
                ) {
                    open_md.push_str(&wrap_teaching_markdown_literal_block(&body_tsv, mode));
                } else {
                    open_md.push_str(&created.prompt);
                }
            } else {
                open_md.push_str(&created.prompt);
            }
            if let Some(exp) =
                teaching_exposure_at(st, created.prompt_hash.as_str(), created.session.as_str())
                    .await
            {
                if let Ok(ctx) = st.catalog.snapshot().load_context(&primary_entry_id) {
                    let entities = plan
                        .seeds_by_entry
                        .get(&primary_entry_id)
                        .cloned()
                        .unwrap_or_default();
                    if let Some(hint) = super::super::seeds::relation_target_deferred_mutator_hint(
                        ctx.cgs.as_ref(),
                        primary_entry_id.as_str(),
                        plasm_context_intent,
                        &exp.all_qualified_entities(),
                        &entities,
                        &exp,
                        match &ranked_capabilities {
                            RankedCapabilitiesArg::Set(opt) => opt.as_deref(),
                            RankedCapabilitiesArg::Unspecified => None,
                        },
                    ) {
                        open_md.push_str(&hint);
                    }
                }
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
            relations_delta: Vec::new(),
        });
        (created.prompt_hash, created.session, true)
    };

    let coord_key = ExecuteCoordKey {
        prompt_hash: prompt_hash.clone(),
        session_id: session_id.clone(),
    };

    if had_binding || hydrated {
        let unchanged = st
            .session_coordination
            .with_exposure_commit(&coord_key, || async {
                apply_ranked_capabilities_session_update(
                    st,
                    prompt_hash.as_str(),
                    session_id.as_str(),
                    &ranked_capabilities,
                )
                .await?;
                let intent_changed = apply_context_intent_session_update(
                    st,
                    prompt_hash.as_str(),
                    session_id.as_str(),
                    plasm_context_intent,
                )
                .await?;
                if let Some(sess_arc) = st.get_execute_session(&prompt_hash, &session_id).await {
                    if let Some(ref exp) = sess_arc.teaching_exposure {
                        let catalogs_ready = plan
                            .process_order
                            .iter()
                            .all(|eid| sess_arc.contexts_by_entry.contains_key(eid));
                        if catalogs_ready
                            && seeds_exposure_ready_for_reuse(exp, &seeds)
                            && !ranked_capabilities_need_exposure_replay(exp, &ranked_capabilities)
                            && !intent_changed
                        {
                            return Ok::<
                                Option<plasm_core::TeachingExposureSession>,
                                super::SessionMutateError,
                            >(Some(exp.clone()));
                        }
                    }
                }
                Ok::<Option<plasm_core::TeachingExposureSession>, super::SessionMutateError>(None)
            })
            .await?;
        if let Some(exp) = unchanged {
            return Ok(ApplyCapabilitySeedsOutcome {
                prompt_hash,
                session_id,
                primary_entry_id: primary_entry_id.clone(),
                principal,
                waves: vec![unchanged_expand_wave(primary_entry_id, Some(&exp))],
                binding_updated,
                new_symbol_space: false,
                stale_execute_binding_recovered,
                stale_binding_previous,
                symbol_space_reset: false,
            });
        }
    }

    let skip_primary_open = binding.is_none() && waves.iter().any(|w| w.mode == "open");
    let federate_expand_waves = commit_federate_and_expand_waves(
        st,
        principal_incoming,
        prompt_hash.as_str(),
        session_id.as_str(),
        &plan,
        skip_primary_open,
        principal.clone(),
        outbound_ref,
        bindings_ref,
    )
    .await?;
    waves.extend(federate_expand_waves);

    if waves.len() > 1
        && waves.iter().all(|w| {
            w.teaching_prompt_chars_added == 0
                && w.markdown_delta.trim().is_empty()
                && w.relations_delta.is_empty()
        })
    {
        let exposure = teaching_exposure_at(st, prompt_hash.as_str(), session_id.as_str()).await;
        waves = vec![unchanged_expand_wave(
            primary_entry_id.clone(),
            exposure.as_ref(),
        )];
    }

    persist_from_execute_row(
        st,
        logical_session_id,
        prompt_hash.as_str(),
        session_id.as_str(),
    )
    .await;

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
        symbol_space_reset,
    })
}
