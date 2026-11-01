//! Explicit review and optional, compile-checked example retrieval.
use crate::mcp_server::request::{McpRequest, ToolFailure};
use crate::mcp_server::{parse_logical_session_ref_arg, PlasmMcpHandler};
use crate::workflow_learning::{self as learning, model::AchievedSubtask, LearningMode};
use rmcp::model::{CallToolResult, ContentBlock, Tool};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;

pub(crate) fn tools() -> Vec<Tool> {
    let learning = learning::WorkflowLearning::default();
    if !learning.enabled() {
        return Vec::new();
    }
    let mut review = Tool::default();
    review.name = "plasm_review".into();
    review.description=Some("Explicitly seal completed subtasks and observed outcomes for background workflow learning. Cite successful run IDs; missing or unresolved execution evidence is rejected. This is an agent-attested review, not a proof of user satisfaction.".into());
    review.input_schema=Arc::new(json!({"type":"object","additionalProperties":false,
        "required":["logical_session_ref","subtasks"],"properties":{
            "logical_session_ref":{"type":"string"},
            "subtasks":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"object","additionalProperties":false,
                "required":["intent","observed_outcome","run_ids"],"properties":{
                    "intent":{"type":"string","minLength":1},"observed_outcome":{"type":"string","minLength":1},
                    "run_ids":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"string"}}
                }}}
        }}).as_object().expect("schema object").clone());
    if learning.mode != LearningMode::Serving {
        return vec![review];
    }
    let mut example = Tool::default();
    example.name = "plasm_example".into();
    example.description=Some("Optionally retrieve a workflow example offered by discovery. Resolves minimal catalogue dependencies, binds current session symbols, and typechecks before returning Python and teaching. Does not execute the example.".into());
    example.input_schema = Arc::new(
        json!({"type":"object","additionalProperties":false,
        "required":["logical_session_ref","example_ref"],"properties":{
            "logical_session_ref":{"type":"string"},"example_ref":{"type":"string","format":"uuid"}
        }})
        .as_object()
        .expect("schema object")
        .clone(),
    );
    vec![review, example]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewArgs {
    logical_session_ref: String,
    subtasks: Vec<AchievedSubtask>,
}

impl PlasmMcpHandler {
    pub(crate) async fn handle_learning_tool(
        &self,
        key: &str,
        runtime: &Arc<McpRequest>,
        v: &serde_json::Value,
        name: &str,
    ) -> Result<CallToolResult, ToolFailure> {
        let principal = self.ensure_mcp_principal(key, runtime).await?;
        let config = self.tenant_mcp_cfg(runtime).await?;
        let tenant = crate::incoming_auth::tenant_scope(principal.as_ref());
        let scope = learning::scope(&tenant, config.as_deref());
        let wire = parse_logical_session_ref_arg(
            if name == "plasm_review" {
                "plasm_review"
            } else {
                "plasm_example"
            },
            v,
        )?;
        let id = self.resolve_logical_session_ref_to_uuid("plasm_review", &wire)?;
        if !self
            .plasm
            .logical_sessions
            .verify_tenant(crate::session_identity::LogicalSessionId(id), &tenant)
            .await
        {
            return Err(ToolFailure::rejected("session unavailable in this scope"));
        }
        let repo = self
            .plasm
            .workflow_learning
            .repository()
            .await
            .map_err(|e| ToolFailure::rejected(e.to_string()))?;
        if repo
            .session_scope(&scope.tenant, crate::session_identity::LogicalSessionId(id))
            .await
            .map_err(|e| ToolFailure::rejected(e.to_string()))?
            .as_ref()
            != Some(&scope)
        {
            return Err(ToolFailure::rejected(
                "session is not bound to this learning project",
            ));
        }
        let binding = self
            .resolve_binding_for_logical(key, id)
            .await
            .ok_or_else(|| ToolFailure::rejected("execute session unavailable"))?;
        let session=self.plasm.sessions.get_by_strs(&binding.prompt_hash,&binding.session_id).await.ok_or_else(||ToolFailure::rejected("execute session is not resident; refresh it with plasm_context before review or example retrieval"))?;
        if name == "plasm_review" {
            let args: ReviewArgs = serde_json::from_value(v.clone())
                .map_err(|e| ToolFailure::rejected(e.to_string()))?;
            if args.logical_session_ref != wire
                || args.subtasks.is_empty()
                || args.subtasks.len() > 16
                || args.subtasks.iter().any(|s| s.run_ids.len() > 32)
            {
                return Err(ToolFailure::rejected("review requires bounded subtasks and complete execution evidence without unresolved operations"));
            }
            let sealed = self
                .plasm
                .workflow_learning
                .seal_review(
                    &self.plasm,
                    learning::ReviewSession {
                        scope,
                        logical_session: crate::session_identity::LogicalSessionId(id),
                        prompt_hash: binding.prompt_hash.clone(),
                        execute_session: binding.session_id.clone(),
                        subtasks: args.subtasks,
                    },
                )
                .await
                .map_err(|error| ToolFailure::rejected(error.to_string()))?;
            self.plasm.workflow_learning.start_worker();
            return Ok(CallToolResult::success(vec![ContentBlock::text(serde_json::to_string(&json!({"review_ref":sealed.id,"evidence_digest":sealed.digest,"status":"queued","assurance":"agent_attested"})).expect("review JSON"))]));
        }
        if self.plasm.workflow_learning.mode != LearningMode::Serving {
            return Err(ToolFailure::rejected("workflow serving is not enabled"));
        }
        let example = v
            .get("example_ref")
            .and_then(|x| x.as_str())
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .ok_or_else(|| ToolFailure::rejected("invalid example_ref"))?;
        let authorization = example_authorization(&self.plasm, &session, config.as_deref());
        let prepared = learning::serving::prepare_example_fetch(
            &self.plasm,
            &repo,
            scope,
            example,
            &binding.prompt_hash,
            &binding.session_id,
            &authorization,
        )
        .await
        .map_err(|e| ToolFailure::rejected(e.to_string()))?;
        let fresh = self.tenant_mcp_cfg(runtime).await?;
        let authorization = example_authorization(&self.plasm, &session, fresh.as_deref());
        let served =
            learning::serving::commit_example_fetch(&self.plasm, &repo, prepared, &authorization)
                .await
                .map_err(|e| ToolFailure::rejected(e.to_string()))?;
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string(&served).map_err(|e| ToolFailure::rejected(e.to_string()))?,
        )]))
    }
}

fn example_authorization(
    host: &crate::server_state::PlasmHostState,
    session: &crate::execute_session::ExecuteSession,
    config: Option<&crate::mcp_runtime_config::McpRuntimeConfig>,
) -> crate::discovery_store::DiscoveryAuthorization {
    use plasm_core::discovery::CgsCatalog;
    let catalogs = config
        .map(|c| c.allowed_entry_ids.iter().cloned().collect())
        .unwrap_or_else(|| {
            host.catalog
                .snapshot()
                .list_entries()
                .into_iter()
                .map(|e| e.entry_id)
                .collect()
        });
    let mut authorization = crate::discovery_store::DiscoveryAuthorization::catalogs(catalogs);
    if let Some(config) = config {
        authorization.capabilities = config
            .capabilities_by_entry
            .iter()
            .filter(|(_, c)| !c.is_empty())
            .map(|(k, v)| (k.clone(), v.iter().cloned().collect()))
            .collect();
    }
    session
        .discovery_pin
        .as_ref()
        .map_or(authorization.clone(), |pin| {
            pin.authorization.intersection(&authorization)
        })
}
