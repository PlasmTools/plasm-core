//! Apply capability seeds.

use super::super::super::*;

use super::super::backend::tenant_outbound_hosted_kv_for_entries;
use super::super::seeds::{
    apply_ranked_capabilities_session_update, build_capability_exposure_plan,
    format_session_unchanged_one_liner, group_seed_entities_by_entry,
    normalize_ranked_capabilities_for_gate, primary_entry_id_for_grouped, seeds_fully_exposed,
    wrap_teaching_markdown_literal_block, RankedCapabilitiesArg, STALE_EXECUTE_BINDING_NOTICE,
};
use super::expand::expand_execute_teaching_session;
use super::federate::federate_execute_session;
use super::open::execute_session_create_response_inner;

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
                    "apply_capability_seeds: MCP execute binding stale (session missing, expired, or catalog reload); opening fresh execute session"
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
                    if let Some(body_tsv) = teaching_tsv_from_wrapped_prompt(
                        &created.prompt,
                        mode.markdown_fence_info_string(),
                        TeachingFenceSlice::AgentFull,
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
