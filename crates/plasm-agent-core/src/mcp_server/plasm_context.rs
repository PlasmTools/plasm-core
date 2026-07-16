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
    accumulated_intent_meta_preview, LogicalSessionId, PlasmContextSessionMode,
};
use crate::trace_hub::PlasmContextTrace;

use super::context_new_seeds;
use super::tool_parse::{
    parse_optional_principal, parse_plasm_context_ranked_capabilities,
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
        let principal = parse_optional_principal(v);
        let tcfg = self.tenant_mcp_cfg(runtime).await?;
        let allowed_ids: Option<Vec<String>> = tcfg.as_ref().map(|cfg| {
            let mut ids: Vec<String> = cfg.allowed_entry_ids.iter().cloned().collect();
            ids.sort();
            ids
        });
        let scope = tenant_scope(principal_incoming.as_ref());

        let optional_seeds = parse_tool_seeds_optional(tname, v)?;

        let rec = match session_mode {
            PlasmContextSessionMode::New => {
                self.plasm
                    .logical_sessions
                    .mint_session(&scope, intent)
                    .await
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
                rec
            }
        };
        let logical_session_ref = format_logical_session_wire_ref(rec.logical_session_id);
        let logical_uuid = rec.logical_session_id.as_uuid();
        let ls_key = logical_uuid.to_string();
        let accumulated_intent = rec.accumulated_intent.as_str();

        let (seeds, auto_ranked_from_selector) = match session_mode {
            PlasmContextSessionMode::New => match context_new_seeds::resolve_context_new_seeds(
                tname,
                self.plasm.catalog.snapshot().as_ref(),
                intent,
                allowed_ids.clone(),
                optional_seeds,
            )
            .await?
            {
                ready @ context_new_seeds::ContextNewSeeds::Ready { .. } => {
                    ready.entities_for_teaching()
                }
                #[cfg(feature = "semantic-auto-seed")]
                context_new_seeds::ContextNewSeeds::Abstain(res) => return Ok(res),
            },
            PlasmContextSessionMode::Extend => {
                match context_new_seeds::resolve_context_extend_seeds(
                    tname,
                    self.plasm.catalog.snapshot().as_ref(),
                    accumulated_intent,
                    allowed_ids.clone(),
                    optional_seeds,
                )
                .await?
                {
                    ready @ context_new_seeds::ContextNewSeeds::Ready { .. } => {
                        ready.entities_for_teaching()
                    }
                    #[cfg(feature = "semantic-auto-seed")]
                    context_new_seeds::ContextNewSeeds::Abstain(res) => return Ok(res),
                }
            }
        };
        let ranked_capabilities = match (ranked_capabilities_arg, auto_ranked_from_selector) {
            (RankedCapabilitiesArg::Set(Some(names)), _) => RankedCapabilitiesArg::Set(Some(names)),
            (RankedCapabilitiesArg::Set(None), Some(auto)) => {
                RankedCapabilitiesArg::Set(Some(auto))
            }
            (other, Some(auto)) => match other {
                RankedCapabilitiesArg::Unspecified => RankedCapabilitiesArg::Set(Some(auto)),
                x => x,
            },
            (x, None) => x,
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
        if session_mode == PlasmContextSessionMode::Extend {
            let live = async {
                if let Some(b) = &binding {
                    if self
                        .plasm
                        .get_execute_session(&b.prompt_hash, &b.session_id)
                        .await
                        .is_some()
                    {
                        return true;
                    }
                }
                if let Some(pair) = self.plasm.logical_execute_bindings.get(&logical_uuid).await {
                    return self
                        .plasm
                        .get_execute_session(&pair.0, &pair.1)
                        .await
                        .is_some();
                }
                false
            }
            .await;
            if !live {
                return Err(CallToolError::from_message(
                    "session_mode \"extend\" requires a live execute session for this logical_session_ref — use session_mode \"new\" or reopen after expiry",
                ));
            }
        }
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
        if session_mode == PlasmContextSessionMode::New {
            churn_advisory = crate::http_execute::format_session_churn_advisory(
                self.plasm.as_ref(),
                &scope,
                Some(rec.logical_session_id),
                &seeds,
            )
            .await;
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
            // Context has no structured UI App yet — ContentOnly keeps one finalize exit.
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
}
