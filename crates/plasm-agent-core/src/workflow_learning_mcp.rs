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
    review.meta = Some(
        json!({"ui":{"visibility":["app"]}})
            .as_object()
            .expect("review metadata")
            .clone()
            .into(),
    );
    review.description=Some("Explicitly seal completed subtasks and observed outcomes for background workflow learning. Cite successful execution IDs from plasm_training action executions; missing or unresolved execution evidence is rejected. This is an agent-attested review, not a proof of user satisfaction.".into());
    review.input_schema=Arc::new(json!({"type":"object","additionalProperties":false,
        "required":["logical_session_ref","subtasks","expected_revision","idempotency_key"],"properties":{
            "logical_session_ref":{"type":"string"},
            "expected_revision":{"type":"integer","minimum":0},"idempotency_key":{"type":"string","format":"uuid"},
            "subtasks":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"object","additionalProperties":false,
                "required":["intent","observed_outcome","execution_ids"],"properties":{
                    "intent":{"type":"string","minLength":1},"observed_outcome":{"type":"string","minLength":1},
                    "execution_ids":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"string"}}
                }}}
        }}).as_object().expect("schema object").clone());
    let mut tools = crate::workflow_training_mcp::tools();
    tools.push(review);
    if learning.mode != LearningMode::Serving {
        return tools;
    }
    let mut example = Tool::default();
    example.name = "plasm_example".into();
    example.description=Some("Optionally fetch a teaching example offered by discovery. Adapt it to your task: distinguish relevant operations from unrelated effects and do not copy unrequested effects. Fetching checks the source and adds its required declarations to this session; examples do not authorize execution.".into());
    example.input_schema = Arc::new(
        json!({"type":"object","additionalProperties":false,
        "required":["logical_session_ref","example_ref"],"properties":{
            "logical_session_ref":{"type":"string"},"example_ref":{"type":"string","format":"uuid"}
        }})
        .as_object()
        .expect("schema object")
        .clone(),
    );
    tools.push(example);
    tools
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewArgs {
    logical_session_ref: String,
    subtasks: Vec<AchievedSubtask>,
    expected_revision: u64,
    idempotency_key: uuid::Uuid,
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
            let actor = crate::workflow_training_mcp::caller_actor(
                scope.clone(),
                principal.as_ref(),
                runtime,
            )?;
            let args: ReviewArgs = serde_json::from_value(v.clone())
                .map_err(|e| ToolFailure::rejected(e.to_string()))?;
            if args.logical_session_ref != wire
                || args.subtasks.is_empty()
                || args.subtasks.len() > 16
                || args.subtasks.iter().any(|s| s.execution_ids.len() > 32)
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
                        actor,
                        expected_revision: args.expected_revision,
                        idempotency_key: args.idempotency_key,
                    },
                )
                .await
                .map_err(|error| ToolFailure::rejected(error.to_string()))?;
            self.plasm.workflow_learning.start_worker();
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string(
                    &json!({"review_ref":sealed.id,"status":"queued","assurance":"agent_attested"}),
                )
                .expect("review JSON"),
            )]));
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
            served.agent_text(),
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
