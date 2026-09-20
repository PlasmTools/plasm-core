//! MCP `plasm_context` tool handler.

use std::sync::Arc;

use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::{CallToolResult, TextContent};
use rust_mcp_sdk::McpServer;
use serde_json::json;
use tracing::Instrument;

use crate::http_execute::{
    apply_capability_seeds, build_plasm_context_agent_markdown, build_plasm_context_tool_meta,
    ApplyCapabilitySeedsOutcome, PlasmContextToolMetaParams,
};
use crate::incoming_auth::tenant_scope;
use crate::mcp_logical_ref::format_logical_session_wire_ref;
use crate::session_identity::{
    accumulated_intent_meta_preview, LogicalSessionId, PlasmContextSessionMode,
};
use crate::trace_hub::PlasmContextTrace;

use super::tool_parse::{
    parse_effect_slots, parse_optional_principal, parse_plasm_context_session_mode,
};
use super::transport::PlasmExecBinding;
use super::{PlasmMcpHandler, MAX_MCP_EXEC_BINDINGS};
use crate::discovery_service::{DiscoveryService, RouteTurn};
use plasm_core::discovery::CgsCatalog;

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
        let effect_slots = parse_effect_slots(tname, v)?;
        let (session_mode, extend_ref) = parse_plasm_context_session_mode(tname, v)?;
        if v.get("seeds").is_some() || v.get("ranked_capabilities").is_some() {
            return Err(CallToolError::invalid_arguments(tname, Some("plasm_context accepts current intent, affirmative effect slots, and continuation fields; explicit seed selection has been removed".into())));
        }
        let principal = parse_optional_principal(v);
        let tcfg = self.tenant_mcp_cfg(runtime).await?;
        let scope = tenant_scope(principal_incoming.as_ref());
        let existing = if session_mode == PlasmContextSessionMode::Extend {
            let wire = extend_ref.as_deref().expect("validated extend ref");
            let id = LogicalSessionId(self.resolve_logical_session_ref_to_uuid(tname, wire)?);
            if !self.plasm.logical_sessions.verify_tenant(id, &scope).await {
                return Err(CallToolError::from_message(
                    "logical_session_ref is unknown or belongs to another tenant",
                ));
            }
            Some(
                self.plasm
                    .logical_sessions
                    .get(id)
                    .await
                    .ok_or_else(|| CallToolError::from_message("logical session expired"))?,
            )
        } else {
            None
        };
        let logical_id = existing
            .as_ref()
            .map(|rec| rec.logical_session_id.as_uuid().to_string());
        let mut exposed = Vec::new();
        let mut session_pin = None;
        if let Some(rec) = &existing {
            session_pin = Some(rec.discovery_pin.clone().ok_or_else(|| {
                CallToolError::from_message("intent extension requires a pinned discovery session")
            })?);
            let id = rec.logical_session_id.as_uuid();
            let binding = self.resolve_binding_for_logical(key, id).await;
            let pair = match binding {
                Some(b) => Some((b.prompt_hash, b.session_id)),
                None => self.plasm.logical_execute_bindings.get(&id).await,
            };
            if let Some(pair) = pair {
                let session = self
                    .plasm
                    .get_execute_session(&pair.0, &pair.1)
                    .await
                    .ok_or_else(|| {
                        CallToolError::from_message(
                            "execute session expired or its pinned generation is unavailable",
                        )
                    })?;
                let execute_pin = session.discovery_pin.as_ref().ok_or_else(|| {
                    CallToolError::from_message("execute session has no discovery pin")
                })?;
                let pin = session_pin.as_mut().expect("validated discovery pin");
                if execute_pin.pin_id != pin.pin_id || execute_pin.generation != pin.generation {
                    return Err(CallToolError::from_message(
                        "execute and discovery pins disagree",
                    ));
                }
                pin.authorization = pin.authorization.intersection(&execute_pin.authorization);
                if let Some(exposure) = &session.teaching_exposure {
                    exposed.extend(exposure.surface.capabilities.iter().map(|cap| {
                        plasm_core::prerequisites::CapabilityRef {
                            catalog: cap.entry_id.clone(),
                            capability: cap.capability.to_string(),
                        }
                    }));
                }
            }
        }
        let generation = match &session_pin {
            Some(pin) => Arc::new(pin.generation.clone()),
            None => self
                .plasm
                .catalog
                .discovery_generation()
                .map_err(|e| CallToolError::from_message(e.to_string()))?,
        };
        let current = self
            .plasm
            .catalog
            .pinned_view(&generation)
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        let allowed = tcfg
            .as_ref()
            .map(|cfg| cfg.allowed_entry_ids.iter().cloned().collect())
            .unwrap_or_else(|| {
                current
                    .snapshot()
                    .list_entries()
                    .into_iter()
                    .map(|entry| entry.entry_id)
                    .collect::<std::collections::BTreeSet<_>>()
            });
        let mut allowed = crate::discovery_store::DiscoveryAuthorization::catalogs(allowed);
        if let Some(cfg) = &tcfg {
            allowed.capabilities = cfg
                .capabilities_by_entry
                .iter()
                .filter(|(_, capabilities)| !capabilities.is_empty())
                .map(|(entry, capabilities)| {
                    (entry.clone(), capabilities.iter().cloned().collect())
                })
                .collect();
        }
        if let Some(pin) = &session_pin {
            allowed = pin.authorization.intersection(&allowed);
        }
        if ["routing_ref", "clarify_choices", "clarify_choice"]
            .iter()
            .any(|key| v.get(*key).is_some())
        {
            return Err(CallToolError::invalid_arguments(
                tname,
                Some("Discovery accepts current intent plus affirmative effect slots; conversational choices belong to the agent".into()),
            ));
        }
        let store = self
            .plasm
            .catalog
            .discovery_store()
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        let provenance = if let Some(id) = logical_id.as_deref() {
            store
                .intent_provenance(id)
                .await
                .and_then(|chain| chain.derived(intent.to_owned()))
        } else {
            crate::intent_provenance::IntentProvenance::from_turns([intent.to_owned()])
        }
        .map_err(|e| CallToolError::from_message(e.to_string()))?;
        let service = DiscoveryService::from_env(store.clone())
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        let receipt = service
            .route_turn(RouteTurn {
                new_generation: &generation,
                intent_provenance: &provenance,
                effect_slots: &effect_slots,
                logical_session: logical_id.as_deref(),
                allowed: &allowed,
                exposed: &exposed,
                expires_at: std::time::SystemTime::now()
                    + std::time::Duration::from_secs(24 * 3600),
            })
            .await
            .map_err(|e| CallToolError::from_message(format!("routing error: {e}")))?;
        let logical_uuid = uuid::Uuid::parse_str(&receipt.pin_id)
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        let rec = self
            .plasm
            .logical_sessions
            .register_routed_session(
                LogicalSessionId(logical_uuid),
                &scope,
                &receipt.intent_provenance,
                Some(crate::discovery_store::DiscoverySessionPin {
                    pin_id: receipt.pin_id.clone(),
                    generation: receipt.retrieval.generation.clone(),
                    authorization: receipt.authorization.clone(),
                }),
            )
            .await
            .map_err(CallToolError::from_message)?;
        if receipt.closure.is_none() {
            let content = if let Some(recovery) = &receipt.recovery {
                recovery.render_unmatched_markdown()
            } else {
                "**plasm_context:** no additional capability matches.".into()
            };
            let wire_ref = format_logical_session_wire_ref(LogicalSessionId(logical_uuid));
            let content = format!("{content}\n\n**logical_session_ref:** `{wire_ref}`");
            let mut meta = serde_json::Map::new();
            meta.insert("logical_session_ref".into(), json!(wire_ref));
            meta.insert(
                "routing".into(),
                serde_json::to_value(&receipt)
                    .map_err(|e| CallToolError::from_message(e.to_string()))?,
            );
            return Ok(crate::mcp_ui_payload::DualLaneToolResult {
                content,
                plasm_meta: meta,
                profile: crate::mcp_delivery::McpDeliveryProfile::ContentOnly,
                inline_plan_ui: None,
            }
            .into_call_tool_result());
        }
        let routed_host = self
            .plasm
            .with_discovery_route(receipt)
            .await
            .map_err(|e| CallToolError::from_message(e.to_string()))?;
        let route = routed_host
            .discovery_route
            .as_ref()
            .expect("validated route");
        let closure = route
            .closure
            .as_ref()
            .expect("validated capability closure");
        let registry = routed_host.catalog.snapshot();
        let mut seeds = Vec::new();
        for reference in closure
            .business
            .iter()
            .chain(&closure.input_sources)
            .chain(&closure.prerequisites)
        {
            let ctx = registry
                .load_context(&reference.catalog)
                .map_err(|e| CallToolError::from_message(e.to_string()))?;
            let cap = ctx
                .cgs
                .capabilities
                .get(reference.capability.as_str())
                .ok_or_else(|| {
                    CallToolError::from_message("selected capability absent from pinned catalog")
                })?;
            seeds.push(crate::http_execute::CapabilitySeed {
                entry_id: reference.catalog.clone(),
                entity: cap.domain.to_string(),
            });
        }
        let logical_session_ref = format_logical_session_wire_ref(rec.logical_session_id);
        let ls_key = logical_uuid.to_string();
        let accumulated_intent = rec.accumulated_intent.as_str();
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
            &routed_host,
            principal_incoming.as_ref(),
            binding
                .as_ref()
                .map(|b| (b.prompt_hash.as_str(), b.session_id.as_str())),
            seeds,
            principal,
            tcfg.clone(),
            Some(logical_uuid),
            accumulated_intent,
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
        let mut text = build_plasm_context_agent_markdown(
            logical_session_ref.as_str(),
            &out.waves,
            out.symbol_space_reset,
            churn_advisory.as_str(),
        );
        if let Some(session) = routed_host
            .get_execute_session(&out.prompt_hash, &out.session_id)
            .await
        {
            if let Some(exposure) = &session.teaching_exposure {
                let catalogs = session
                    .contexts_by_entry
                    .iter()
                    .map(|(id, ctx)| (id.clone(), ctx.cgs.as_ref()))
                    .collect();
                let symbols = exposure.to_symbol_map();
                let guidance = plasm_core::prompt_render::render_prerequisite_bindings(
                    closure,
                    &catalogs,
                    symbols.as_ref(),
                )
                .map_err(CallToolError::from_message)?;
                if !guidance.is_empty() {
                    text.push_str("\n\n");
                    text.push_str(&guidance);
                }
            }
        }
        debug_assert!(
            route.recovery.is_none(),
            "complete route cannot carry recovery"
        );
        text = format!("{}\n\n{text}", route.intent_analysis);
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
        let mut plasm = build_plasm_context_tool_meta(
            &out,
            PlasmContextToolMetaParams {
                logical_session_ref: logical_session_ref.as_str(),
                session_mode: session_mode.as_str(),
                intent_turns: rec.intent_provenance.turns().count(),
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
        plasm.insert(
            "routing".into(),
            serde_json::to_value(route.as_ref())
                .map_err(|e| CallToolError::from_message(e.to_string()))?,
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
}
