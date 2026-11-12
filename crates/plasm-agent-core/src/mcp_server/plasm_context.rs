//! MCP `plasm_context` tool handler.

use std::sync::Arc;

use crate::mcp_server::request::McpRequest;
use crate::mcp_server::request::ToolFailure;
use rmcp::model::CallToolResult;
use serde_json::json;
use tracing::Instrument;

use crate::http_execute::{
    ApplyCapabilitySeedsOutcome, PlasmContextToolMetaParams, apply_capability_seeds_for_mcp,
    build_plasm_context_agent_markdown, build_plasm_context_tool_meta,
};
use crate::incoming_auth::tenant_scope;
use crate::mcp_logical_ref::format_logical_session_wire_ref;
use crate::session_identity::{
    LogicalSessionId, PlasmContextSessionMode, accumulated_intent_meta_preview,
};
use crate::trace_hub::PlasmContextTrace;

use super::tool_parse::{parse_optional_principal, parse_plasm_context_session_mode};
use super::transport::PlasmExecBinding;
use super::{MAX_MCP_EXEC_BINDINGS, PlasmMcpHandler};
use crate::discovery_service::{DiscoveryService, RouteTurn};
use plasm_core::discovery::CgsCatalog;

#[derive(Default)]
pub(super) struct DiscoveryGates(
    tokio::sync::Mutex<
        std::collections::HashMap<uuid::Uuid, std::sync::Weak<tokio::sync::Mutex<()>>>,
    >,
);

impl DiscoveryGates {
    async fn for_session(&self, id: uuid::Uuid) -> Arc<tokio::sync::Mutex<()>> {
        let mut gates = self.0.lock().await;
        gates.retain(|_, gate| gate.strong_count() > 0);
        match gates.get(&id).and_then(std::sync::Weak::upgrade) {
            Some(gate) => gate,
            None => {
                let gate = Arc::new(tokio::sync::Mutex::new(()));
                gates.insert(id, Arc::downgrade(&gate));
                gate
            }
        }
    }
}

impl PlasmMcpHandler {
    pub(crate) async fn handle_mcp_tool_plasm_context(
        &self,
        key: &str,
        runtime: &Arc<McpRequest>,
        v: &serde_json::Value,
    ) -> Result<CallToolResult, ToolFailure> {
        let mut previous = None;
        let (_, extend_ref) = parse_plasm_context_session_mode("plasm_context", v)?;
        if let Some(wire) = extend_ref.as_deref() {
            if let Ok(id) = self.resolve_logical_session_ref_to_uuid("plasm_context", wire) {
                let principal = self.ensure_mcp_principal(key, runtime).await?;
                let scope = tenant_scope(principal.as_ref());
                if self
                    .plasm
                    .logical_sessions
                    .verify_tenant(LogicalSessionId(id), &scope)
                    .await
                    && self
                        .plasm
                        .workflow_learning
                        .deferred_recalls
                        .has_delivery(&id.to_string(), false)
                {
                    if let Some(record) =
                        self.plasm.logical_sessions.get(LogicalSessionId(id)).await
                    {
                        let mut request = v.clone();
                        if let Some(args) = request.as_object_mut() {
                            args.insert("intent".into(), json!(record.intent_provenance.current()));
                            if let Ok(update) = self
                                .handle_mcp_tool_plasm_context_locked(key, runtime, &request, true)
                                .await
                            {
                                previous = Some(update);
                            }
                        }
                    }
                }
            }
        }
        let mut result = match self
            .handle_mcp_tool_plasm_context_locked(key, runtime, v, false)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                if let Some(update) = previous {
                    let mut content = vec![rmcp::model::ContentBlock::text(
                        "Completed discovery for the previous request:",
                    )];
                    content.extend(update.content);
                    content.push(rmcp::model::ContentBlock::text(format!(
                        "Current discovery failed: {error}"
                    )));
                    return Ok(CallToolResult::error(content));
                }
                return Err(error);
            }
        };
        if let Some(update) = previous {
            result.content.push(rmcp::model::ContentBlock::text(
                "Completed discovery for the previous request:",
            ));
            result.content.extend(update.content);
        }
        let ready = v
            .get("logical_session_ref")
            .and_then(|value| value.as_str())
            .and_then(|wire| {
                self.resolve_logical_session_ref_to_uuid("plasm_context", wire)
                    .ok()
            })
            .is_some_and(|id| {
                self.plasm
                    .workflow_learning
                    .deferred_recalls
                    .has_delivery(&id.to_string(), false)
            });
        if ready {
            if let Ok(update) = self
                .handle_mcp_tool_plasm_context_locked(key, runtime, v, true)
                .await
            {
                result.content.extend(update.content);
            }
        }
        Ok(result)
    }

    pub(crate) async fn handle_mcp_supplemental_context(
        &self,
        key: &str,
        runtime: &Arc<McpRequest>,
        v: &serde_json::Value,
    ) -> Result<CallToolResult, ToolFailure> {
        self.handle_mcp_tool_plasm_context_locked(key, runtime, v, true)
            .await
    }

    async fn handle_mcp_tool_plasm_context_locked(
        &self,
        key: &str,
        runtime: &Arc<McpRequest>,
        v: &serde_json::Value,
        supplemental_only: bool,
    ) -> Result<CallToolResult, ToolFailure> {
        if !matches!(
            self.plasm.workflow_learning.mode,
            crate::workflow_learning::LearningMode::Serving
                | crate::workflow_learning::LearningMode::ShadowWithRecall
        ) {
            return self
                .handle_mcp_tool_plasm_context_inner(key, runtime, v, supplemental_only)
                .await;
        }
        let (_, extend_ref) = parse_plasm_context_session_mode("plasm_context", v)?;
        let Some(wire) = extend_ref else {
            return self
                .handle_mcp_tool_plasm_context_inner(key, runtime, v, supplemental_only)
                .await;
        };
        let id = self.resolve_logical_session_ref_to_uuid("plasm_context", &wire)?;
        let principal = self.ensure_mcp_principal(key, runtime).await?;
        let scope = tenant_scope(principal.as_ref());
        if !self
            .plasm
            .logical_sessions
            .verify_tenant(LogicalSessionId(id), &scope)
            .await
        {
            return Err(ToolFailure::rejected(
                "logical_session_ref is unknown or belongs to another tenant",
            ));
        }
        let gate = self.discovery_gates.for_session(id).await;
        let _guard = gate.lock().await;
        self.handle_mcp_tool_plasm_context_inner(key, runtime, v, supplemental_only)
            .await
    }

    async fn handle_mcp_tool_plasm_context_inner(
        &self,
        key: &str,
        runtime: &Arc<McpRequest>,
        v: &serde_json::Value,
        supplemental_only: bool,
    ) -> Result<CallToolResult, ToolFailure> {
        let tname = "plasm_context";
        let principal_incoming = self.ensure_mcp_principal(key, runtime).await?;
        let intent = v.get("intent").and_then(|x| x.as_str()).ok_or_else(|| {
            ToolFailure::invalid_arguments(tname, Some("missing `intent`".into()))
        })?;
        let (session_mode, extend_ref) = parse_plasm_context_session_mode(tname, v)?;
        let training = v
            .get("training")
            .map(|value| {
                serde_json::from_value::<crate::workflow_training_wire::CreationTrainingIntent>(
                    value.clone(),
                )
            })
            .transpose()
            .map_err(|error| ToolFailure::invalid_arguments(tname, Some(error.to_string())))?;
        if training.is_some() && session_mode == PlasmContextSessionMode::Extend {
            return Err(ToolFailure::invalid_arguments(tname, Some("creation training intent requires session_mode new; use plasm_training for later opt-in".into())));
        }

        if v.get("seeds").is_some()
            || v.get("ranked_capabilities").is_some()
            || v.get("effect_slots").is_some()
        {
            return Err(ToolFailure::invalid_arguments(tname, Some("plasm_context accepts current intent and continuation fields; explicit seed selection has been removed".into())));
        }
        let principal = parse_optional_principal(v);
        let tcfg = self.tenant_mcp_cfg(runtime).await?;
        let scope = tenant_scope(principal_incoming.as_ref());
        let existing = if session_mode == PlasmContextSessionMode::Extend {
            let wire = extend_ref.as_deref().expect("validated extend ref");
            let id = LogicalSessionId(self.resolve_logical_session_ref_to_uuid(tname, wire)?);
            if !self.plasm.logical_sessions.verify_tenant(id, &scope).await {
                return Err(ToolFailure::rejected(
                    "logical_session_ref is unknown or belongs to another tenant",
                ));
            }
            Some(
                self.plasm
                    .logical_sessions
                    .get(id)
                    .await
                    .ok_or_else(|| ToolFailure::rejected("logical session expired"))?,
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
                ToolFailure::rejected("intent extension requires a pinned discovery session")
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
                        ToolFailure::rejected(
                            "execute session expired or its pinned generation is unavailable",
                        )
                    })?;
                let execute_pin = session
                    .discovery_pin
                    .as_ref()
                    .ok_or_else(|| ToolFailure::rejected("execute session has no discovery pin"))?;
                let pin = session_pin.as_mut().expect("validated discovery pin");
                if execute_pin.pin_id != pin.pin_id || execute_pin.generation != pin.generation {
                    return Err(ToolFailure::rejected("execute and discovery pins disagree"));
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
                .map_err(|e| ToolFailure::rejected(e.to_string()))?,
        };
        let current = self
            .plasm
            .catalog
            .pinned_view(&generation)
            .await
            .map_err(|e| ToolFailure::rejected(e.to_string()))?;
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
            return Err(ToolFailure::invalid_arguments(
                tname,
                Some(
                    "Discovery accepts current intent; conversational choices belong to the agent"
                        .into(),
                ),
            ));
        }
        let store = self
            .plasm
            .catalog
            .discovery_store()
            .await
            .map_err(|e| ToolFailure::rejected(e.to_string()))?;
        let (provenance, _recall_continuation) = if let Some(id) = logical_id.as_deref() {
            let chain = store
                .intent_provenance(id)
                .await
                .map_err(|error| ToolFailure::rejected(error.to_string()))?;
            let continuation = chain.current() == intent;
            (
                if supplemental_only {
                    if !continuation {
                        return Err(ToolFailure::rejected(
                            "supplemental discovery intent is stale",
                        ));
                    }
                    chain
                } else {
                    chain
                        .derived(intent.to_owned())
                        .map_err(|error| ToolFailure::rejected(error.to_string()))?
                },
                continuation,
            )
        } else {
            (
                crate::intent_provenance::IntentProvenance::from_turns([intent.to_owned()])
                    .map_err(|error| ToolFailure::rejected(error.to_string()))?,
                false,
            )
        };
        let learning_scope = crate::workflow_learning::scope(&scope, tcfg.as_deref());
        use crate::workflow_learning::deferred_recall::{RecallContext, RecallStatus};
        let learning = &self.plasm.workflow_learning;
        let recall_context = RecallContext {
            scope: learning_scope,
            intent: intent.to_owned(),
            intent_revision: crate::workflow_learning::deferred_recall::intent_revision(
                &provenance,
            ),
            generation: generation.to_string(),
            authorization: allowed.clone(),
        };
        if !supplemental_only {
            if let Some(session) = logical_id.as_deref() {
                learning.deferred_recalls.discard_stale_supplemental(
                    session,
                    &recall_context,
                    &exposed,
                );
            }
        }
        let supplemental_unavailable = !supplemental_only
            && logical_id.as_deref().is_some_and(|session| {
                learning
                    .deferred_recalls
                    .take_supplemental_failure(session, &recall_context)
            });
        let mut supplemental_claim = if supplemental_only {
            let session = logical_id.as_deref().ok_or_else(|| {
                ToolFailure::rejected("supplemental discovery requires an existing session")
            })?;
            let claim = learning
                .deferred_recalls
                .claim_supplemental(session, &recall_context)
                .ok_or_else(|| ToolFailure::rejected("supplemental discovery is not ready"))?;
            let expected = claim.result().associations.len();
            let checked = crate::workflow_learning::deferred_recall::validate_current(
                learning,
                &recall_context.scope,
                (Vec::new(), claim.result().associations.clone()),
            )
            .await
            .ok_or_else(|| {
                ToolFailure::rejected("supplemental discovery revalidation unavailable")
            })?;
            if checked.1.len() != expected {
                claim.commit();
                return Err(ToolFailure::rejected(
                    "supplemental discovery association is no longer current",
                ));
            }
            Some(claim)
        } else {
            None
        };
        let status = logical_id
            .as_deref()
            .map(|id| learning.deferred_recalls.status(id, &recall_context))
            .unwrap_or(RecallStatus::Missing);
        let mut recall_ticket = None;
        let (examples, _associations, learning_unavailable, learning_pending) = if supplemental_only
        {
            (Vec::new(), Vec::new(), false, false)
        } else {
            match status {
                RecallStatus::Completed((examples, associations)) => {
                    match crate::workflow_learning::deferred_recall::validate_current(
                        learning,
                        &recall_context.scope,
                        (examples, associations),
                    )
                    .await
                    {
                        Some((examples, associations)) => (examples, associations, false, false),
                        None => (Vec::new(), Vec::new(), true, false),
                    }
                }
                RecallStatus::Pending => (Vec::new(), Vec::new(), false, true),
                RecallStatus::Unavailable => (Vec::new(), Vec::new(), true, false),
                RecallStatus::Missing => {
                    let prior = logical_id
                        .as_deref()
                        .map(|session| {
                            learning
                                .deferred_recalls
                                .prior_associations_for_session(session, &recall_context)
                        })
                        .unwrap_or_default();
                    recall_ticket = learning.deferred_recalls.start(
                        Arc::clone(learning),
                        recall_context.clone(),
                        store.clone(),
                        prior,
                        exposed.clone(),
                    );
                    let expected = matches!(
                        learning.mode,
                        crate::workflow_learning::LearningMode::Serving
                            | crate::workflow_learning::LearningMode::ShadowWithRecall
                    );
                    (
                        Vec::new(),
                        Vec::new(),
                        expected && recall_ticket.is_none(),
                        recall_ticket.is_some(),
                    )
                }
            }
        };
        let receipt = if let Some(claim) = supplemental_claim.as_ref() {
            let mut receipt: crate::discovery_service::RoutingReceipt =
                serde_json::from_str(&claim.result().receipt_json)
                    .map_err(|error| ToolFailure::rejected(error.to_string()))?;
            if receipt.retrieval.generation != *generation || receipt.authorization != allowed {
                return Err(ToolFailure::rejected(
                    "supplemental discovery scope or catalogue changed",
                ));
            }
            receipt.pin_id = logical_id.clone().expect("supplemental session checked");
            receipt.intent_provenance = provenance.clone();
            receipt.intent = intent.to_owned();
            receipt
        } else {
            let service = DiscoveryService::from_env(store.clone())
                .map_err(|e| ToolFailure::rejected(e.to_string()))?;
            service
                .route_turn(RouteTurn {
                    new_generation: &generation,
                    intent_provenance: &provenance,
                    logical_session: logical_id.as_deref(),
                    allowed: &allowed,
                    exposed: &exposed,
                    expires_at: std::time::SystemTime::now()
                        + std::time::Duration::from_secs(24 * 3600),
                })
                .await
                .map_err(|e| ToolFailure::rejected(format!("routing error: {e}")))?
        };
        if let Some(ticket) = recall_ticket {
            learning.deferred_recalls.bind(ticket, &receipt.pin_id);
        }
        let logical_uuid = uuid::Uuid::parse_str(&receipt.pin_id)
            .map_err(|e| ToolFailure::rejected(e.to_string()))?;
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
            .map_err(|error| ToolFailure::rejected(error.to_string()))?;
        if receipt.closure.is_none() {
            let content = if let Some(recovery) = &receipt.recovery {
                recovery.render_unmatched_markdown()
            } else if receipt.search.has_unavailable_provider() {
                "**plasm_context:** a later capability assessment was unavailable; retry discovery to continue.".into()
            } else if receipt.search.has_pending_operation_judgments() {
                "**plasm_context:** capability discovery has more candidates to assess. Extend this session with the same intent to continue.".into()
            } else if receipt.search.has_incomplete_operation_judgments() {
                "**plasm_context:** some candidate contributions are uncertain; refine the intent to continue discovery.".into()
            } else if !receipt.search.deferred.is_empty() {
                "**plasm_context:** some candidates could not be assessed within discovery limits; refine the intent to continue.".into()
            } else {
                "**plasm_context:** no additional capability matches.".into()
            };
            let wire_ref = format_logical_session_wire_ref(LogicalSessionId(logical_uuid));
            let pending = if learning_pending {
                "\nOptional teaching is being prepared. To check it, call plasm_context with session_mode extend, this logical_session_ref, and the same intent."
            } else {
                ""
            };
            let unavailable = if supplemental_unavailable {
                "\nSupplemental discovery is unavailable; ordinary discovery remains available."
            } else {
                ""
            };
            let content = format!(
                "{}\n\n{content}\n\n**logical_session_ref:** `{wire_ref}`{pending}{unavailable}",
                receipt.intent_analysis
            );
            let mut meta = serde_json::Map::new();
            meta.insert("logical_session_ref".into(), json!(wire_ref));
            meta.insert(
                "routing".into(),
                serde_json::to_value(&receipt).map_err(|e| ToolFailure::rejected(e.to_string()))?,
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
            .map_err(|e| ToolFailure::rejected(e.to_string()))?;
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
                .map_err(|e| ToolFailure::rejected(e.to_string()))?;
            let cap = ctx
                .cgs
                .capabilities
                .get(reference.capability.as_str())
                .ok_or_else(|| {
                    ToolFailure::rejected("selected capability absent from pinned catalog")
                })?;
            seeds.push(crate::http_execute::CapabilitySeed {
                entry_id: reference.catalog.clone(),
                entity: cap.domain.to_string(),
            });
        }
        let logical_session_ref = format_logical_session_wire_ref(rec.logical_session_id);
        let ls_key = logical_uuid.to_string();
        if self.plasm.workflow_learning.enabled() {
            let learning_scope = crate::workflow_learning::scope(&scope, tcfg.as_deref());
            match self.plasm.workflow_learning.repository().await {
                Ok(repository) => {
                    if let Err(error) = repository
                        .bind_session(&learning_scope, rec.logical_session_id)
                        .await
                    {
                        tracing::warn!(target: "plasm_agent::workflow_learning", %error, "session learning scope unavailable");
                    }
                    if session_mode == PlasmContextSessionMode::New {
                        if let Ok(actor) = crate::workflow_training_mcp::caller_actor(
                            learning_scope.clone(),
                            principal_incoming.as_ref(),
                            runtime,
                        ) {
                            if let Err(error) = repository
                                .bind_training_controller(&actor, rec.logical_session_id)
                                .await
                            {
                                tracing::warn!(%error, "training controller unavailable; discovery continues");
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(target: "plasm_agent::workflow_learning", %error, "learning unavailable; discovery continues")
                }
            }
            self.plasm.workflow_learning.start_worker();
        }
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
                    return Err(ToolFailure::rejected(format!(
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
                session_churn = Some(adv);
            }
        }
        if supplemental_only {
            let latest = store
                .intent_provenance(&ls_key)
                .await
                .map_err(|error| ToolFailure::rejected(error.to_string()))?;
            if crate::workflow_learning::deferred_recall::intent_revision(&latest)
                != recall_context.intent_revision
            {
                return Err(ToolFailure::rejected(
                    "supplemental discovery intent changed before exposure",
                ));
            }
            let current_record = self
                .plasm
                .logical_sessions
                .get(LogicalSessionId(logical_uuid))
                .await
                .ok_or_else(|| ToolFailure::rejected("supplemental discovery session expired"))?;
            let pin = current_record.discovery_pin.ok_or_else(|| {
                ToolFailure::rejected("supplemental discovery session pin is missing")
            })?;
            if pin.generation != *generation {
                return Err(ToolFailure::rejected(
                    "supplemental discovery catalogue changed",
                ));
            }
            let mut current = pin.authorization;
            if let Some(config) = self.tenant_mcp_cfg(runtime).await? {
                let mut configured = crate::discovery_store::DiscoveryAuthorization::catalogs(
                    config.allowed_entry_ids.iter().cloned().collect(),
                );
                configured.capabilities = config
                    .capabilities_by_entry
                    .iter()
                    .filter(|(_, values)| !values.is_empty())
                    .map(|(entry, values)| (entry.clone(), values.iter().cloned().collect()))
                    .collect();
                current = current.intersection(&configured);
            }
            if current != allowed {
                return Err(ToolFailure::rejected(
                    "supplemental discovery authorization changed",
                ));
            }
        }
        let out: ApplyCapabilitySeedsOutcome = apply_capability_seeds_for_mcp(
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
        .map_err(|err| ToolFailure::cause(std::io::Error::other(err.to_string())))?;

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
            "",
        );
        if !examples.is_empty() {
            text.push_str("\nOptional workflow examples (retrieve with plasm_example):\n");
            for example in &examples {
                text.push_str(&format!(
                    "- {}: {}\n",
                    example.id, example.candidate.intent.purpose
                ));
            }
        }
        if examples.is_empty() {
            for item in learning
                .deferred_recalls
                .completed_for_session(
                    &logical_uuid.to_string(),
                    &recall_context.scope,
                    &generation,
                    &allowed,
                )
                .into_iter()
                .filter(|item| !item.examples.is_empty())
                .take(3)
            {
                if let Some((current, _)) =
                    crate::workflow_learning::deferred_recall::validate_current(
                        learning,
                        &recall_context.scope,
                        (item.examples, item.associations),
                    )
                    .await
                {
                    let fresh = learning.deferred_recalls.claim_delivery(
                        item.ticket,
                        &current.iter().map(|example| example.id).collect::<Vec<_>>(),
                    );
                    if !fresh.is_empty() {
                        text.push_str(&format!(
                            "\nOptional workflow examples for {} (retrieve with plasm_example):\n",
                            item.intent
                        ));
                        for example in current
                            .iter()
                            .filter(|example| fresh.contains(&example.id))
                            .take(3)
                        {
                            text.push_str(&format!(
                                "- {}: {}\n",
                                example.id, example.candidate.intent.purpose
                            ));
                        }
                    }
                }
            }
        }
        if learning_unavailable {
            text.push_str("\nOptional teaching examples are unavailable for this request; ordinary discovery continues.\n");
        }
        if supplemental_unavailable {
            text.push_str(
                "\nSupplemental discovery is unavailable; ordinary discovery remains available.\n",
            );
        }
        if learning_pending {
            text.push_str("\nOptional teaching is being prepared and will appear on a later session response when ready.\n");
        }
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
                .map_err(|error| ToolFailure::rejected(error.to_string()))?;
                if !guidance.is_empty() {
                    text.push_str("\n\n");
                    text.push_str(&guidance);
                }
            }
        }
        if let Some(recovery) = &route.recovery {
            text.push_str("\n\n");
            text.push_str(&recovery.render_unmatched_markdown());
        }
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
        if let Some(training) = training {
            let actor = crate::workflow_training_mcp::caller_actor(
                crate::workflow_learning::scope(&scope, tcfg.as_deref()),
                principal_incoming.as_ref(),
                runtime,
            );
            let status = match actor {
                Ok(actor) => {
                    crate::workflow_training_wire::request_on_creation(
                        &self.plasm,
                        &actor,
                        rec.logical_session_id,
                        &training,
                    )
                    .await
                }
                Err(error) => {
                    json!({"capture_availability":"unavailable", "error":error.to_string()})
                }
            };
            text.push_str(&format!("\nTraining: {}\n", status));
            plasm.insert("training".into(), status);
        }
        if !examples.is_empty() {
            plasm.insert(
                "workflow_examples".into(),
                json!(examples
                    .iter()
                    .map(|example| {
                        let program = example.candidate.program();
                        json!({"example_ref":example.id,"purpose":example.candidate.intent.purpose,
                    "outputs":program.map(|p| p.outputs.iter().map(|output| output.summary()).collect::<Vec<_>>()),
                    "assurance":example.candidate.quality.assurance})
                    })
                    .collect::<Vec<_>>()),
            );
        }
        let text = crate::mcp_agent_present::AgentContent::context(
            &crate::mcp_agent_present::ContextTokenRefs {
                logical_session_ref: logical_session_ref.as_str(),
            },
            &text,
        )
        .render();
        let res = if plasm.is_empty() {
            CallToolResult::success(vec![rmcp::model::ContentBlock::text(text)])
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
        if let Some(claim) = supplemental_claim.take() {
            tracing::debug!(target: "plasm_agent::workflow_learning", wave_count = out.waves.len(), "supplemental discovery delivered to MCP session");
            claim.commit();
        }
        Ok(res)
    }
}

#[cfg(test)]
mod discovery_gate_tests {
    use super::*;

    #[tokio::test]
    async fn same_session_serializes_while_other_sessions_remain_independent() {
        let gates = DiscoveryGates::default();
        let id = uuid::Uuid::new_v4();
        let first = gates.for_session(id).await;
        let same = gates.for_session(id).await;
        let other = gates.for_session(uuid::Uuid::new_v4()).await;
        let held = first.lock().await;
        assert!(same.try_lock().is_err());
        assert!(other.try_lock().is_ok());
        drop(held);
        assert!(same.try_lock().is_ok());
    }
}
