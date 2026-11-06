//! MCP presentation of the shared opt-in training service.

use std::sync::Arc;

use rmcp::model::{CallToolResult, ContentBlock, Tool};
use serde_json::json;

use crate::mcp_server::request::{McpRequest, ToolFailure};
use crate::mcp_server::{parse_logical_session_ref_arg, PlasmMcpHandler};
use crate::workflow_learning::{
    self as learning,
    training::{TrainingError, TrustedTrainingActor},
};
use crate::workflow_training_wire::{
    approval_url, candidate_page, decode_candidate_cursor, parse_candidate_ref, training_failure,
    CandidateCommand, SessionCommand, TrainingStatusResponse,
};

pub(crate) fn tools() -> Vec<Tool> {
    let mut tool = Tool::default();
    tool.name = "plasm_training".into();
    tool.meta = Some(
        json!({"ui":{"resourceUri":crate::mcp_app::TRAINING.uri,"visibility":["app"]}})
            .as_object()
            .expect("training metadata")
            .clone()
            .into(),
    );
    tool.description = Some("Read training status or completed training executions, explicitly request prospective training, stop capture and hold unpublished material, or resume future capture. Ordinary sessions can use examples without training. Requests require an active delegation or authenticated human consent; this tool cannot approve consent or publication.".into());
    tool.input_schema = Arc::new(json!({"type":"object","additionalProperties":false,"required":["logical_session_ref","action"],"properties":{
        "logical_session_ref":{"type":"string"},"action":{"enum":["status","executions","execution","request","stop","resume"]},"execution_id":{"type":"string"},
        "expected_revision":{"type":"integer","minimum":0},"idempotency_key":{"type":"string","format":"uuid"}
    },"allOf":[{"if":{"properties":{"action":{"enum":["request","stop","resume"]}}},"then":{"required":["expected_revision","idempotency_key"]}}]}).as_object().expect("training schema").clone());
    let mut candidates = Tool::default();
    candidates.name = "plasm_training_candidates".into();
    candidates.description = Some("Inspect whole-program training candidates, request authenticated human decisions for selected immutable revisions, or read a pending decision. Requesting a decision does not approve or publish anything.".into());
    candidates.meta = tool.meta.clone();
    candidates.input_schema = Arc::new(json!({"type":"object","additionalProperties":false,"required":["logical_session_ref","action"],"properties":{
        "logical_session_ref":{"type":"string"},"action":{"enum":["list","read","request_decision","request_status","request_release","request_execution_release"]},
        "limit":{"type":"integer","minimum":1,"maximum":32,"default":16},"cursor":{"type":"string"},
        "candidate_ref":{"type":"string"},"request_ref":{"type":"string","format":"uuid"},
        "execution_ids":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"string"}},
        "review_refs":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"string","format":"uuid"}},
        "expected_revision":{"type":"integer","minimum":0},"idempotency_key":{"type":"string","format":"uuid"},
        "selection":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"object","additionalProperties":false,"required":["review_ref","candidate_hash","guidance_digest","action"],"properties":{
            "review_ref":{"type":"string","format":"uuid"},"candidate_hash":{"type":"string"},"guidance_digest":{"type":"string"},
            "action":{"type":"object","additionalProperties":false,"required":["decision"],"properties":{"decision":{"enum":["approve","reject","request_revision"]},"reason":{"type":"string"}}}
        }}}
    }}).as_object().expect("candidate schema").clone());
    vec![tool, candidates]
}

#[cfg(test)]
mod model_visibility_tests {
    #[test]
    fn training_controls_are_app_only() {
        for tool in super::tools() {
            let value = serde_json::to_value(&tool).expect("serialize MCP tool");
            assert_eq!(
                value["_meta"]["ui"]["visibility"],
                serde_json::json!(["app"])
            );
        }
    }
}

fn failure(error: TrainingError) -> CallToolResult {
    let (_, failure) = training_failure(&error);
    CallToolResult::error(vec![ContentBlock::text(
        serde_json::to_string(&failure).expect("training error serializes"),
    )])
}

pub(crate) fn caller_actor(
    scope: learning::model::LibraryScope,
    principal: Option<&crate::incoming_auth::TenantPrincipal>,
    runtime: &McpRequest,
) -> Result<TrustedTrainingActor, ToolFailure> {
    let principal = principal.ok_or_else(|| {
        ToolFailure::rejected(
            "authentication_required: training controls require an authenticated caller",
        )
    })?;
    let credential =
        match runtime.identity.as_ref() {
            Some(identity) if identity.anonymous => {
                return Err(ToolFailure::rejected(
                    "authentication_required: anonymous access does not authorize training",
                ))
            }
            Some(identity) => {
                if let Some(key) = identity.api_key_id {
                    format!("mcp-key:{key}")
                } else if let Some(client) = &identity.oauth_client_id {
                    format!("mcp-oauth:{client}:{}", principal.subject)
                } else {
                    return Err(ToolFailure::rejected(
                        "authentication_required: individual credential identity is unavailable",
                    ));
                }
            }
            None => return Err(ToolFailure::rejected(
                "authentication_required: individual transport credential identity is unavailable",
            )),
        };
    TrustedTrainingActor::authenticated(scope, principal.subject.clone(), credential)
        .map_err(ToolFailure::cause)
}

impl PlasmMcpHandler {
    pub(crate) async fn handle_training_candidates(
        &self,
        key: &str,
        runtime: &Arc<McpRequest>,
        args: &serde_json::Value,
    ) -> Result<CallToolResult, ToolFailure> {
        let principal = self.ensure_mcp_principal(key, runtime).await?;
        let config = self.tenant_mcp_cfg(runtime).await?;
        let tenant = crate::incoming_auth::tenant_scope(principal.as_ref());
        let scope = learning::scope(&tenant, config.as_deref());
        let actor = caller_actor(scope.clone(), principal.as_ref(), runtime)?;
        let wire = parse_logical_session_ref_arg("plasm_training_candidates", args)?;
        let logical = crate::mcp_logical_ref::parse_logical_session_wire_ref(&wire)
            .map_err(ToolFailure::cause)?;
        if !self
            .plasm
            .logical_sessions
            .verify_tenant(logical, &tenant)
            .await
        {
            return Ok(failure(TrainingError::Unavailable));
        }
        let repo = self
            .plasm
            .workflow_learning
            .repository()
            .await
            .map_err(ToolFailure::cause)?;
        if repo
            .session_scope(&scope.tenant, logical)
            .await
            .map_err(ToolFailure::cause)?
            .as_ref()
            != Some(&scope)
        {
            return Ok(failure(TrainingError::Unavailable));
        }
        if self
            .resolve_binding_for_logical(key, logical.as_uuid())
            .await
            .is_none()
        {
            return Ok(failure(TrainingError::Unavailable));
        }
        if let Err(error) = repo.check_training_controller(&actor, logical).await {
            return Ok(failure(error));
        }
        let mut payload = args
            .as_object()
            .cloned()
            .ok_or_else(|| ToolFailure::invalid_arguments("plasm_training_candidates", None))?;
        payload.remove("logical_session_ref");
        let command: CandidateCommand = serde_json::from_value(payload.into()).map_err(|e| {
            ToolFailure::invalid_arguments("plasm_training_candidates", Some(e.to_string()))
        })?;
        let value = match command {
            CandidateCommand::List { limit, cursor } => {
                let after = decode_candidate_cursor(cursor.as_deref(), &scope, logical)
                    .map_err(ToolFailure::cause)?;
                let page = repo
                    .list_training_candidates(&scope, logical, after.as_ref(), limit)
                    .await
                    .map_err(ToolFailure::cause)?;
                serde_json::to_value(
                    candidate_page(page, &scope, logical).map_err(ToolFailure::cause)?,
                )
                .map_err(ToolFailure::cause)?
            }
            CandidateCommand::Read { candidate_ref } => {
                let reference = parse_candidate_ref(&candidate_ref).map_err(ToolFailure::cause)?;
                if repo
                    .training_candidate_session(&scope, &reference)
                    .await
                    .map_err(ToolFailure::cause)?
                    != logical
                {
                    return Ok(failure(TrainingError::Unavailable));
                }
                serde_json::to_value(
                    repo.read_training_candidate(&scope, &reference)
                        .await
                        .map_err(ToolFailure::cause)?
                        .agent_view(),
                )
                .map_err(ToolFailure::cause)?
            }
            CandidateCommand::RequestDecision {
                expected_revision,
                idempotency_key,
                selection,
            } => {
                let request = match repo
                    .request_candidate_decision(
                        &actor,
                        logical,
                        expected_revision,
                        idempotency_key,
                        selection,
                    )
                    .await
                {
                    Ok(request) => request,
                    Err(error) => return Ok(failure(error)),
                };
                json!({"approval_url":approval_url(request.request_ref),"request":request})
            }
            CandidateCommand::RequestStatus { request_ref } => {
                let request = match repo.read_training_request(&actor, request_ref).await {
                    Ok(request) => request,
                    Err(error) => return Ok(failure(error)),
                };
                if request.logical_session != logical {
                    return Ok(failure(TrainingError::Unavailable));
                }
                json!({"approval_url":approval_url(request.request_ref),"request":request})
            }
            CandidateCommand::RequestExecutionRelease {
                expected_revision,
                idempotency_key,
                execution_ids,
            } => {
                let request = match repo
                    .request_training_execution_release(
                        &actor,
                        logical,
                        expected_revision,
                        idempotency_key,
                        execution_ids,
                    )
                    .await
                {
                    Ok(request) => request,
                    Err(error) => return Ok(failure(error)),
                };
                json!({"approval_url":approval_url(request.request_ref),"request":request})
            }
            CandidateCommand::RequestRelease {
                expected_revision,
                idempotency_key,
                review_refs,
            } => {
                let request = match repo
                    .request_training_release(
                        &actor,
                        logical,
                        expected_revision,
                        idempotency_key,
                        review_refs,
                    )
                    .await
                {
                    Ok(request) => request,
                    Err(error) => return Ok(failure(error)),
                };
                json!({"approval_url":approval_url(request.request_ref),"request":request})
            }
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string(&value).map_err(ToolFailure::cause)?,
        )]))
    }

    pub(crate) async fn handle_training_tool(
        &self,
        key: &str,
        runtime: &Arc<McpRequest>,
        args: &serde_json::Value,
    ) -> Result<CallToolResult, ToolFailure> {
        let principal = self.ensure_mcp_principal(key, runtime).await?;
        let config = self.tenant_mcp_cfg(runtime).await?;
        let tenant = crate::incoming_auth::tenant_scope(principal.as_ref());
        let scope = learning::scope(&tenant, config.as_deref());
        let actor = caller_actor(scope.clone(), principal.as_ref(), runtime)?;
        let wire = parse_logical_session_ref_arg("plasm_training", args)?;
        let logical = crate::mcp_logical_ref::parse_logical_session_wire_ref(&wire)
            .map_err(ToolFailure::cause)?;
        if !self
            .plasm
            .logical_sessions
            .verify_tenant(logical, &tenant)
            .await
        {
            return Ok(failure(TrainingError::Unavailable));
        }
        let repo = self
            .plasm
            .workflow_learning
            .repository()
            .await
            .map_err(ToolFailure::cause)?;
        if repo
            .session_scope(&scope.tenant, logical)
            .await
            .map_err(ToolFailure::cause)?
            .as_ref()
            != Some(&scope)
        {
            return Ok(failure(TrainingError::Unavailable));
        }
        if self
            .resolve_binding_for_logical(key, logical.as_uuid())
            .await
            .is_none()
        {
            return Ok(failure(TrainingError::Unavailable));
        }
        if let Err(error) = repo.check_training_controller(&actor, logical).await {
            return Ok(failure(error));
        }
        let mut payload = args
            .as_object()
            .cloned()
            .ok_or_else(|| ToolFailure::invalid_arguments("plasm_training", None))?;
        payload.remove("logical_session_ref");
        if payload.get("action").and_then(|v| v.as_str()) == Some("executions") {
            if payload.len() != 1 {
                return Ok(failure(TrainingError::InvalidCommand));
            }
            let executions = repo
                .execution_index(&scope, logical)
                .await
                .map_err(ToolFailure::cause)?;
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string(&json!({"executions": executions}))
                    .map_err(ToolFailure::cause)?,
            )]));
        }
        if payload.get("action").and_then(|v| v.as_str()) == Some("execution") {
            if payload.len() != 2 {
                return Ok(failure(TrainingError::InvalidCommand));
            }
            let Some(value) = payload.get("execution_id") else {
                return Ok(failure(TrainingError::InvalidCommand));
            };
            let execution_id = serde_json::from_value(value.clone())
                .map_err(|_| ToolFailure::invalid_arguments("plasm_training", None))?;
            let receipt = repo
                .execution_receipt(&scope, logical, execution_id)
                .await
                .map_err(ToolFailure::cause)?;
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string(&json!({
                    "execution_id": receipt.execution_id,
                    "source": receipt.source,
                    "teaching_plan": receipt.teaching_plan,
                }))
                .map_err(ToolFailure::cause)?,
            )]));
        }
        let status = if payload.get("action").and_then(|v| v.as_str()) == Some("status") {
            if payload.len() != 1 {
                return Ok(failure(TrainingError::InvalidCommand));
            }
            repo.training_status(&scope, logical).await
        } else {
            let command: SessionCommand = serde_json::from_value(payload.into()).map_err(|e| {
                ToolFailure::invalid_arguments("plasm_training", Some(e.to_string()))
            })?;
            repo.training_command(&actor, logical, &command.envelope())
                .await
        };
        match status {
            Ok(status) => Ok(CallToolResult::success(vec![ContentBlock::text(
                serde_json::to_string(&TrainingStatusResponse::from(status))
                    .map_err(ToolFailure::cause)?,
            )])),
            Err(error) => Ok(failure(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn training_tools_expose_app_without_human_resolution() {
        for tool in tools() {
            assert!(tool.meta.is_some());
            let schema = serde_json::to_value(&tool.input_schema).unwrap();
            let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
            assert!(!actions.iter().any(|value| value
                .as_str()
                .is_some_and(|value| value.starts_with("resolve") || value == "approve")));
        }
    }

    #[test]
    fn transport_key_rotation_changes_actor_credential_without_changing_project() {
        let scope = learning::model::LibraryScope {
            tenant: "tenant:a".into(),
            project: "p".into(),
        };
        let principal = crate::incoming_auth::TenantPrincipal {
            tenant_id: "a".into(),
            subject: "agent".into(),
            method: crate::incoming_auth::IncomingAuthMethod::ApiKey,
        };
        let config = uuid::Uuid::new_v4();
        let actor = |key| {
            let request = McpRequest {
                transport_key: "stream".into(),
                client: crate::mcp_client_info::McpClientInfo::Default,
                identity: Some(crate::mcp_stream_identity::McpTransportIdentity::api_key(
                    "a".into(),
                    "agent".into(),
                    config,
                    key,
                )),
            };
            caller_actor(scope.clone(), Some(&principal), &request).unwrap()
        };
        let first = actor(uuid::Uuid::new_v4());
        let second = actor(uuid::Uuid::new_v4());
        assert_eq!(first.scope(), second.scope());
        assert_ne!(first.credential(), second.credential());
        assert!(!first.may_administer());
        assert!(!first.is_human_owner());
        let anonymous = McpRequest {
            transport_key: "stream".into(),
            client: crate::mcp_client_info::McpClientInfo::Default,
            identity: None,
        };
        assert!(caller_actor(scope, Some(&principal), &anonymous).is_err());
    }
}
