//! MCP `plasm_context` tool handler.

use std::sync::Arc;

use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::{CallToolResult, TextContent};
use rust_mcp_sdk::McpServer;
use serde_json::json;
use tracing::Instrument;

use crate::http_execute::{
    apply_capability_seeds, build_plasm_context_agent_markdown, build_plasm_context_tool_meta,
    ApplyCapabilitySeedsOutcome, PlasmContextToolMetaParams, RankedCapabilitiesArg,
};
use crate::incoming_auth::tenant_scope;
use crate::mcp_logical_ref::format_logical_session_wire_ref;
use crate::session_identity::{
    accumulated_intent_meta_preview, LogicalSessionId, LogicalSessionRecord,
    PlasmContextSessionMode,
};
use crate::trace_hub::PlasmContextTrace;

use super::context_new_seeds::{self, ContextPhase, ContextRouteDecision, SeedsPolicy};
use super::tool_parse::{
    parse_optional_principal, parse_plasm_context_clarify_choice,
    parse_plasm_context_ranked_capabilities, parse_plasm_context_routing_ref,
    parse_plasm_context_session_mode, parse_tool_seeds_optional,
};
use super::transport::PlasmExecBinding;
use super::{PlasmMcpHandler, MAX_MCP_EXEC_BINDINGS};

impl PlasmMcpHandler {
    pub(crate) async fn handle_mcp_tool_plasm_context(
        &self,
        key: &str,
        runtime: &Arc<dyn McpServer>,
        v: &serde_json::Value,
    ) -> Result<CallToolResult, CallToolError> {
        let tname = "plasm_context";
        let principal_incoming = self.ensure_mcp_principal(key, runtime).await?;
        let intent = v.get("intent").and_then(|x| x.as_str()).ok_or_else(|| {
            CallToolError::invalid_arguments(tname, Some("missing `intent`".into()))
        })?;
        let (session_mode, extend_ref) = parse_plasm_context_session_mode(tname, v)?;
        let ranked_capabilities_arg = parse_plasm_context_ranked_capabilities(tname, v)?;
        let routing_ref_arg = parse_plasm_context_routing_ref(tname, v)?;
        let clarify_choice_arg = parse_plasm_context_clarify_choice(tname, v)?;
        let principal = parse_optional_principal(v);
        let tcfg = self.tenant_mcp_cfg(runtime).await?;
        let allowed_ids: Option<Vec<String>> = tcfg.as_ref().map(|cfg| {
            let mut ids: Vec<String> = cfg.allowed_entry_ids.iter().cloned().collect();
            ids.sort();
            ids
        });
        let scope = tenant_scope(principal_incoming.as_ref());
        let optional_seeds = parse_tool_seeds_optional(tname, v)?;

        // --- Route-before-commit: resolve seeds first; mint/append only on Expand/Noop ---
        let (rec, seeds, auto_ranked_from_selector) = match session_mode {
            PlasmContextSessionMode::New => {
                let decision = self
                    .route_context_turn(RouteContextTurn {
                        tool: tname,
                        intent,
                        phase: ContextPhase::New,
                        allowed_ids: allowed_ids.clone(),
                        optional_seeds,
                        logical_session_ref: None,
                        logical_session_id: None,
                        routing_ref: routing_ref_arg.as_deref(),
                        clarify_choice: clarify_choice_arg.as_deref(),
                    })
                    .await?;
                let (seeds, auto_ranked) = match decision {
                    #[cfg(feature = "semantic-auto-seed")]
                    ContextRouteDecision::Abstain(plan) => {
                        return Ok(context_new_seeds::present_abstain(plan, intent));
                    }
                    #[cfg(feature = "semantic-auto-seed")]
                    ContextRouteDecision::Noop => {
                        return Err(CallToolError::from_message(
                            "internal: delta_noop is only valid for session_mode extend",
                        ));
                    }
                    expand @ ContextRouteDecision::Expand { .. } => expand.into_expand(),
                };
                let rec = self
                    .plasm
                    .logical_sessions
                    .mint_session(&scope, intent)
                    .await;
                (rec, seeds, auto_ranked)
            }
            PlasmContextSessionMode::Extend => {
                let wire = extend_ref
                    .as_deref()
                    .expect("extend ref validated in parse");
                let logical_uuid = self.resolve_logical_session_ref_to_uuid(tname, wire)?;
                if !self
                    .plasm
                    .logical_sessions
                    .verify_tenant(LogicalSessionId(logical_uuid), &scope)
                    .await
                {
                    return Err(CallToolError::from_message(
                        "logical_session_ref is unknown or does not belong to this tenant scope",
                    ));
                }
                let id = LogicalSessionId(logical_uuid);
                let Some(existing) = self.plasm.logical_sessions.get(id).await else {
                    return Err(CallToolError::from_message(
                        "logical_session_ref is unknown or expired: use session_mode \"new\" to start a fresh session",
                    ));
                };
                let logical_session_ref =
                    format_logical_session_wire_ref(existing.logical_session_id);

                let binding = self.resolve_binding_for_logical(key, logical_uuid).await;
                let live_es = {
                    let mut found = None;
                    if let Some(b) = &binding {
                        found = self
                            .plasm
                            .get_execute_session(&b.prompt_hash, &b.session_id)
                            .await;
                    }
                    if found.is_none() {
                        if let Some(pair) =
                            self.plasm.logical_execute_bindings.get(&logical_uuid).await
                        {
                            found = self.plasm.get_execute_session(&pair.0, &pair.1).await;
                        }
                    }
                    found
                };
                if live_es.is_none() {
                    return Err(CallToolError::from_message(
                        "session_mode \"extend\" requires a live execute session for this logical_session_ref — use session_mode \"new\" or reopen after expiry",
                    ));
                }
                let exposed: Vec<(String, String)> = live_es
                    .as_ref()
                    .and_then(|es| es.teaching_exposure.as_ref())
                    .map(|exp| {
                        exp.all_qualified_entities()
                            .into_iter()
                            .map(|k| (k.entry_id, k.entity.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                let decision = self
                    .route_context_turn(RouteContextTurn {
                        tool: tname,
                        intent,
                        phase: ContextPhase::Extend {
                            exposed: exposed.as_slice(),
                        },
                        allowed_ids: allowed_ids.clone(),
                        optional_seeds,
                        logical_session_ref: Some(logical_session_ref.as_str()),
                        logical_session_id: Some(logical_uuid),
                        routing_ref: routing_ref_arg.as_deref(),
                        clarify_choice: clarify_choice_arg.as_deref(),
                    })
                    .await?;

                match decision {
                    #[cfg(feature = "semantic-auto-seed")]
                    ContextRouteDecision::Abstain(plan) => {
                        return Ok(context_new_seeds::present_abstain(plan, intent));
                    }
                    #[cfg(feature = "semantic-auto-seed")]
                    ContextRouteDecision::Noop => {
                        let Some(rec) = self
                            .plasm
                            .logical_sessions
                            .append_intent_turn(id, intent)
                            .await
                        else {
                            return Err(CallToolError::from_message(
                                "logical_session_ref is unknown or expired: use session_mode \"new\" to start a fresh session",
                            ));
                        };
                        return Ok(present_delta_noop(&rec, &logical_session_ref));
                    }
                    expand @ ContextRouteDecision::Expand { .. } => {
                        let (seeds, auto_ranked) = expand.into_expand();
                        let Some(rec) = self
                            .plasm
                            .logical_sessions
                            .append_intent_turn(id, intent)
                            .await
                        else {
                            return Err(CallToolError::from_message(
                                "logical_session_ref is unknown or expired: use session_mode \"new\" to start a fresh session",
                            ));
                        };
                        if seeds.is_empty() {
                            return Ok(present_delta_noop(&rec, &logical_session_ref));
                        }
                        (rec, seeds, auto_ranked)
                    }
                }
            }
        };
        let logical_session_ref = format_logical_session_wire_ref(rec.logical_session_id);
        let logical_uuid = rec.logical_session_id.as_uuid();
        let ls_key = logical_uuid.to_string();
        let accumulated_intent = rec.accumulated_intent.as_str();

        // Agent-explicit ranked (emit) always wins; host auto-seed only fills when unspecified.
        let ranked_capabilities = match (ranked_capabilities_arg, auto_ranked_from_selector) {
            (arg, _) if arg.emit_diagnostics() => arg,
            (_, Some(auto)) => RankedCapabilitiesArg::host(Some(auto)),
            (other, None) => other,
        };
        let seeds = crate::http_execute::resolve_capability_seeds(
            seeds,
            self.plasm.catalog.snapshot().as_ref(),
            allowed_ids.as_deref(),
        )
        .map_err(CallToolError::from_message)?;
        let distinct_entries: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for s in &seeds {
                if seen.insert(s.entry_id.clone()) {
                    out.push(s.entry_id.clone());
                }
            }
            out
        };
        if let Some(ref cfg) = tcfg {
            for eid in &distinct_entries {
                if !cfg.entry_allowed(eid) {
                    return Err(CallToolError::from_message(format!(
                        "entry_id not allowed by tenant MCP configuration: {eid}"
                    )));
                }
            }
        }
        let binding = self.resolve_binding_for_logical(key, logical_uuid).await;
        tracing::debug!(
            target: "plasm_agent::mcp",
            tool = tname,
            logical_session_ref = %logical_session_ref,
            logical_session_id = %ls_key,
            mcp_execute_binding_present = binding.is_some(),
            "MCP plasm_context: Plasm execute binding before apply_capability_seeds (false means open path; true means expand/federate against existing prompt_hash/session)"
        );
        let context_span = crate::spans::mcp_tool_plasm_context(logical_session_ref.as_str());
        let mut churn_advisory = String::new();
        let mut session_churn: Option<crate::http_execute::SessionChurnAdvisory> = None;
        if session_mode == PlasmContextSessionMode::New {
            if let Some(adv) = crate::http_execute::format_session_churn_advisory(
                self.plasm.as_ref(),
                &scope,
                Some(rec.logical_session_id),
                &seeds,
                accumulated_intent,
            )
            .await
            {
                churn_advisory = adv.markdown.clone();
                session_churn = Some(adv);
            }
        }
        let out: ApplyCapabilitySeedsOutcome = apply_capability_seeds(
            self.plasm.as_ref(),
            principal_incoming.as_ref(),
            binding
                .as_ref()
                .map(|b| (b.prompt_hash.as_str(), b.session_id.as_str())),
            seeds,
            principal,
            tcfg.clone(),
            Some(logical_uuid),
            accumulated_intent,
            ranked_capabilities,
        )
        .instrument(context_span)
        .await
        .map_err(|err| CallToolError::new(std::io::Error::other(err.to_string())))?;

        if out.stale_execute_binding_recovered {
            self.plasm.trace_hub.finalize_mcp_session(&ls_key).await;
        }

        if out.binding_updated {
            {
                let mut g = self.session_states.write().await;
                if g.len() >= MAX_MCP_EXEC_BINDINGS && !g.contains_key(key) {
                    if let Some(victim) = g.keys().next().cloned() {
                        tracing::warn!(
                            victim = %victim,
                            limit = MAX_MCP_EXEC_BINDINGS,
                            "evicting MCP transport slot to respect soft cap"
                        );
                        g.remove(&victim);
                    }
                }
            }
            let ls = self.logical_mutex(key, &ls_key).await;
            let mut g = ls.lock().await;
            g.binding = Some(PlasmExecBinding {
                prompt_hash: out.prompt_hash.clone(),
                session_id: out.session_id.clone(),
            });
            drop(g);
            self.plasm
                .logical_execute_bindings
                .insert(
                    logical_uuid,
                    out.prompt_hash.clone(),
                    out.session_id.clone(),
                )
                .await;
        }
        let trace_meta = self.trace_session_meta(key, runtime).await;
        self.plasm
            .trace_hub
            .ensure_logical_session(&ls_key, Some(key), trace_meta)
            .await;

        let total_teaching_chars: u64 = out
            .waves
            .iter()
            .map(|w| w.teaching_prompt_chars_added)
            .sum();
        let exposed_entities: usize = out
            .waves
            .iter()
            .flat_map(|w| {
                w.entities
                    .iter()
                    .map(|entity| format!("{}:{entity}", w.entry_id))
            })
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let catalog_count = {
            let mut ids = std::collections::BTreeSet::new();
            for w in &out.waves {
                ids.insert(w.entry_id.as_str());
            }
            ids.len()
        };
        tracing::info!(
            target: "plasm_agent::mcp",
            tool = "plasm_context",
            logical_session_ref = %logical_session_ref,
            exposed_entities,
            catalog_count,
            response_teaching_chars = total_teaching_chars,
            wave_count = out.waves.len(),
            "MCP plasm_context response telemetry"
        );
        let text = build_plasm_context_agent_markdown(
            logical_session_ref.as_str(),
            &out.waves,
            out.symbol_space_reset,
            churn_advisory.as_str(),
        );
        for wave in &out.waves {
            if wave.teaching_prompt_chars_added > 0 {
                let ls = self.logical_mutex(key, &ls_key).await;
                let mut g = ls.lock().await;
                g.stats.teaching_prompt_chars = g
                    .stats
                    .teaching_prompt_chars
                    .saturating_add(wave.teaching_prompt_chars_added);
            }
            self.plasm
                .trace_hub
                .trace_record_plasm_context(
                    &ls_key,
                    PlasmContextTrace {
                        teaching_prompt_chars_added: wave.teaching_prompt_chars_added,
                        reused_session: wave.reused_session,
                        mode: wave.mode.clone(),
                        entry_id: Some(wave.entry_id.clone()),
                        entities: wave.entities.clone(),
                        seeds: wave
                            .entities
                            .iter()
                            .map(|e| format!("{}:{e}", wave.entry_id))
                            .collect(),
                    },
                )
                .await;
        }
        let (domain_revision, relations, symbol_map_fingerprint) = if let Some(sess_arc) = self
            .plasm
            .sessions
            .get_by_strs(&out.prompt_hash, &out.session_id)
            .await
        {
            let rel = sess_arc
                .teaching_exposure
                .as_ref()
                .map(|exposure| exposure.exposed_relation_symbol_rows())
                .filter(|rows| !rows.is_empty())
                .map(|rows| json!(rows));
            (
                Some(sess_arc.domain_revision),
                rel,
                crate::symbol_map_resolve::symbol_map_fingerprint_for_session(sess_arc.as_ref()),
            )
        } else {
            (None, None, None)
        };
        let relations_delta = {
            let deltas: Vec<_> = out
                .waves
                .iter()
                .flat_map(|w| w.relations_delta.iter().cloned())
                .collect();
            if deltas.is_empty() {
                None
            } else {
                Some(json!(deltas))
            }
        };
        let plasm = build_plasm_context_tool_meta(
            &out,
            PlasmContextToolMetaParams {
                logical_session_ref: logical_session_ref.as_str(),
                session_mode: session_mode.as_str(),
                intent_turns: rec.intent_turns.len(),
                accumulated_intent_preview: accumulated_intent_meta_preview(
                    accumulated_intent,
                    240,
                )
                .as_str(),
                domain_revision,
                symbol_map_fingerprint,
                relations,
                relations_delta,
                session_churn: session_churn.as_ref(),
            },
        );
        let text = crate::mcp_agent_present::AgentContent::context(
            &crate::mcp_agent_present::ContextTokenRefs {
                logical_session_ref: logical_session_ref.as_str(),
            },
            &text,
        )
        .render();
        let res = if plasm.is_empty() {
            CallToolResult::text_content(vec![TextContent::new(text, None, None)])
        } else {
            crate::mcp_ui_payload::DualLaneToolResult {
                content: text,
                plasm_meta: plasm,
                profile: crate::mcp_delivery::McpDeliveryProfile::ContentOnly,
                inline_plan_ui: None,
            }
            .into_call_tool_result()
        };
        self.persist_transport_state(key).await;
        Ok(res)
    }

    /// Route seeds for the current turn (no mint/append).
    async fn route_context_turn(
        &self,
        args: RouteContextTurn<'_>,
    ) -> Result<ContextRouteDecision, CallToolError> {
        let catalog = self.plasm.catalog.snapshot();
        let policy = if context_new_seeds::semantic_auto_seed_on() {
            #[cfg(feature = "semantic-auto-seed")]
            {
                let _ = args.optional_seeds;
                SeedsPolicy::Auto(context_new_seeds::AutoSeedRouteArgs {
                    tool: args.tool,
                    intent: args.intent,
                    logical_session_ref: args.logical_session_ref,
                    logical_session_id: args.logical_session_id,
                    allowed_entry_ids: args.allowed_ids.clone(),
                    pending_clarify: self.plasm.pending_clarify.as_ref(),
                    routing_ref: args.routing_ref,
                    clarify_choice: args.clarify_choice,
                })
            }
            #[cfg(not(feature = "semantic-auto-seed"))]
            {
                let _ = (
                    args.logical_session_ref,
                    args.logical_session_id,
                    args.routing_ref,
                    args.clarify_choice,
                );
                SeedsPolicy::Explicit(args.optional_seeds)
            }
        } else {
            let _ = (
                args.logical_session_ref,
                args.logical_session_id,
                args.routing_ref,
                args.clarify_choice,
            );
            SeedsPolicy::Explicit(args.optional_seeds)
        };
        context_new_seeds::resolve_context_seeds(
            args.tool,
            catalog.as_ref(),
            args.intent,
            args.allowed_ids,
            args.phase,
            policy,
        )
        .await
    }
}

struct RouteContextTurn<'a> {
    tool: &'a str,
    intent: &'a str,
    phase: ContextPhase<'a>,
    allowed_ids: Option<Vec<String>>,
    optional_seeds: Option<Vec<crate::http_execute::CapabilitySeed>>,
    logical_session_ref: Option<&'a str>,
    logical_session_id: Option<uuid::Uuid>,
    routing_ref: Option<&'a str>,
    clarify_choice: Option<&'a str>,
}

fn present_delta_noop(rec: &LogicalSessionRecord, logical_session_ref: &str) -> CallToolResult {
    let text = format!(
        "Session already exposes the requested surface — no teaching delta.\n\n`logical_session_ref`: `{logical_session_ref}`\n\nReuse this ref for `plasm` / `plasm_run`, or `extend` again with a new catalog/entity intent."
    );
    let mut plasm = serde_json::Map::new();
    plasm.insert(
        "logical_session_ref".into(),
        serde_json::json!(logical_session_ref),
    );
    plasm.insert("session_mode".into(), serde_json::json!("extend"));
    plasm.insert(
        "intent_turns".into(),
        serde_json::json!(rec.intent_turns.len()),
    );
    plasm.insert("delta_noop".into(), serde_json::json!(true));
    let text = crate::mcp_agent_present::AgentContent::context(
        &crate::mcp_agent_present::ContextTokenRefs {
            logical_session_ref,
        },
        &text,
    )
    .render();
    crate::mcp_ui_payload::DualLaneToolResult {
        content: text,
        plasm_meta: plasm,
        profile: crate::mcp_delivery::McpDeliveryProfile::ContentOnly,
        inline_plan_ui: None,
    }
    .into_call_tool_result()
}
