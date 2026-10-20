//! MCP `resources/read` for Plasm run snapshots and MCP App UI bundles.

use std::sync::Arc;
use std::time::Instant;

use crate::mcp_server::request::McpRequest;
use rmcp::model::{ErrorData, ReadResourceRequestParams, ReadResourceResult};

use crate::run_artifacts::{
    parse_plasm_execute_plan_uri, parse_plasm_session_short_plan_uri,
    strip_plasm_resource_read_source,
};

use super::artifact_resolve;
use super::resource_helpers::read_resource_result_for_payload;
use super::resource_read_trace;
use super::transport::PlasmExecBinding;
use super::PlasmMcpHandler;

async fn resolve_session_binding(
    handler: &PlasmMcpHandler,
    runtime: &Arc<McpRequest>,
    uri: &str,
    read_source: Option<&str>,
    started: Instant,
) -> Result<(PlasmExecBinding, String), ErrorData> {
    let logical_uuid =
        artifact_resolve::logical_uuid_from_session_scoped_uri(uri).map_err(map_resolve_rpc_err)?;
    let ls_key = logical_uuid.to_string();
    let transport_key = Some(runtime.transport_key.clone());
    let binding = if let Some(ref tk) = transport_key {
        handler.resolve_binding_for_logical(tk, logical_uuid).await
    } else {
        handler.resolve_binding_stateless(logical_uuid).await
    };
    let Some(b) = binding else {
        resource_read_trace::McpResourceReadTrace::error(
            Some(ls_key.as_str()),
            read_source,
            started,
            uri,
            None,
            "no_binding",
        )
        .emit(&handler.plasm)
        .await;
        return Err(ErrorData::invalid_params("execute session unavailable: extend plasm_context with the same logical_session_ref and current intent", None));
    };
    Ok((b, ls_key))
}

async fn resolve_session_scoped_run(
    handler: &PlasmMcpHandler,
    runtime: &Arc<McpRequest>,
    uri: &str,
    read_source: Option<&str>,
    started: Instant,
) -> Result<artifact_resolve::ResolvedRunArtifact, ErrorData> {
    let (b, ls_key) = resolve_session_binding(handler, runtime, uri, read_source, started).await?;
    let lookup = artifact_resolve::lookup_from_artifact_uri(uri, &b, ls_key.as_str())
        .map_err(map_resolve_rpc_err)?;
    artifact_resolve::resolve_run_artifact_for_binding(
        handler.plasm.as_ref(),
        &b,
        lookup,
        Some(ls_key.as_str()),
        read_source,
        started,
        uri,
    )
    .await
    .map_err(map_resolve_rpc_err)
}

async fn resolve_session_scoped_plan(
    handler: &PlasmMcpHandler,
    runtime: &Arc<McpRequest>,
    uri: &str,
    read_source: Option<&str>,
    started: Instant,
) -> Result<artifact_resolve::ResolvedCodePlanArtifact, ErrorData> {
    let (_, plan_index) = parse_plasm_session_short_plan_uri(uri).ok_or_else(|| {
        ErrorData::invalid_params(format!("unsupported resource URI: {uri}"), None)
    })?;
    let (b, ls_key) = resolve_session_binding(handler, runtime, uri, read_source, started).await?;
    artifact_resolve::resolve_code_plan_for_binding(
        handler.plasm.as_ref(),
        &b,
        artifact_resolve::CodePlanLookup::Index(plan_index),
        Some(ls_key.as_str()),
        read_source,
        started,
        uri,
    )
    .await
    .map_err(map_resolve_rpc_err)
}

fn map_resolve_rpc_err(e: artifact_resolve::RunArtifactResolveError) -> ErrorData {
    match e {
        artifact_resolve::RunArtifactResolveError::DecodeFailed(msg) => {
            ErrorData::internal_error(format!("run artifact decode failed: {msg}"), None)
        }
        artifact_resolve::RunArtifactResolveError::Integrity(msg) => {
            ErrorData::internal_error(format!("run artifact integrity check failed: {msg}"), None)
        }
        other => ErrorData::invalid_params(other.to_string(), None),
    }
}

async fn read_run_result(
    handler: &PlasmMcpHandler,
    uri: &str,
    resolved: artifact_resolve::ResolvedRunArtifact,
    read_source: Option<&str>,
) -> Result<ReadResourceResult, ErrorData> {
    let payload = if read_source == Some(plasm_trace::MCP_RESOURCE_READ_SOURCE_RUN_EXPLORER_UI) {
        crate::run_artifacts::project_artifact_payload_for_agent(&resolved.payload, false).map_err(
            |error| {
                ErrorData::internal_error(format!("run artifact projection failed: {error}"), None)
            },
        )?
    } else {
        super::resource_helpers::run_artifact_context_payload(
            handler,
            &resolved.prompt_hash,
            &resolved.session_id,
            &resolved.payload,
        )
        .await
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?
    };
    read_resource_result_for_payload(uri, payload)
}

pub(crate) async fn handle_read_resource_request(
    handler: &PlasmMcpHandler,
    params: ReadResourceRequestParams,
    runtime: Arc<McpRequest>,
) -> Result<ReadResourceResult, ErrorData> {
    let started = Instant::now();
    let raw_uri = params.uri.trim();
    let (uri_owned, read_source) = strip_plasm_resource_read_source(raw_uri);
    let uri = uri_owned.as_str();
    let read_source = read_source.as_deref();
    if let Some(bundle) = crate::mcp_app::bundle_for_uri(uri) {
        let Some((content, result_meta)) = crate::mcp_app::read_resource_text(uri) else {
            return Err(ErrorData::invalid_params(
                format!("unknown ui resource: {uri}"),
                None,
            ));
        };
        crate::metrics::record_mcp_resource_read(
            bundle.resource_metric,
            "success",
            "none",
            started.elapsed(),
        );
        return Ok({
            let mut model = ReadResourceResult::new(vec![]);
            model.contents = vec![content];
            model.meta = Some(result_meta.into());
            model
        });
    }

    if uri.starts_with("plasm://session/") {
        if parse_plasm_session_short_plan_uri(uri).is_some() {
            let resolved =
                resolve_session_scoped_plan(handler, &runtime, uri, read_source, started).await?;
            crate::spans::mcp_resource_read().in_scope(|| {
                tracing::info!(
                    target: "plasm_agent::mcp",
                    uri = %uri,
                    bytes = resolved.payload.bytes.len(),
                    "MCP resources/read code plan"
                );
            });
            crate::metrics::record_mcp_resource_read(
                "code_plan",
                "success",
                "none",
                started.elapsed(),
            );
            return read_resource_result_for_payload(uri, resolved.payload);
        }
        let resolved =
            resolve_session_scoped_run(handler, &runtime, uri, read_source, started).await?;
        crate::spans::mcp_resource_read().in_scope(|| {
            tracing::info!(
                target: "plasm_agent::mcp",
                uri = %uri,
                run_id = ?resolved.run_id.as_ref().map(|r| r.to_wire()),
                bytes = resolved.payload.bytes.len(),
                "MCP resources/read"
            );
        });
        crate::metrics::record_mcp_resource_read("canonical", "success", "none", started.elapsed());
        return read_run_result(handler, uri, resolved, read_source).await;
    }

    if let Some((prompt_hash, session_id, plan_id)) = parse_plasm_execute_plan_uri(uri) {
        let ls_key_opt = handler
            .plasm
            .logical_session_id_for_execute_binding(prompt_hash.as_str(), session_id.as_str())
            .await
            .map(|u| u.to_string());
        let binding = PlasmExecBinding {
            prompt_hash: prompt_hash.clone(),
            session_id: session_id.clone(),
        };
        let resolved = artifact_resolve::resolve_code_plan_for_binding(
            handler.plasm.as_ref(),
            &binding,
            artifact_resolve::CodePlanLookup::Id(plan_id),
            ls_key_opt.as_deref(),
            read_source,
            started,
            uri,
        )
        .await
        .map_err(map_resolve_rpc_err)?;
        crate::spans::mcp_resource_read().in_scope(|| {
            tracing::info!(
                target: "plasm_agent::mcp",
                uri = %uri,
                prompt_hash = %prompt_hash,
                session_id = %session_id,
                plan_id = %plan_id,
                bytes = resolved.payload.bytes.len(),
                "MCP resources/read code plan"
            );
        });
        crate::metrics::record_mcp_resource_read("code_plan", "success", "none", started.elapsed());
        return read_resource_result_for_payload(uri, resolved.payload);
    }

    let Some((prompt_hash, session_id, run_id)) =
        crate::run_artifacts::parse_plasm_execute_run_uri(uri)
    else {
        crate::metrics::record_mcp_resource_read(
            "unsupported",
            "error",
            "unsupported_uri",
            started.elapsed(),
        );
        return Err(ErrorData::invalid_params(
            format!("unsupported resource URI: {uri}"),
            None,
        ));
    };
    let ls_key_opt = handler
        .plasm
        .logical_session_id_for_execute_binding(prompt_hash.as_str(), session_id.as_str())
        .await
        .map(|u| u.to_string());
    let binding = PlasmExecBinding {
        prompt_hash: prompt_hash.clone(),
        session_id: session_id.clone(),
    };
    let resolved = artifact_resolve::resolve_run_artifact_for_binding(
        handler.plasm.as_ref(),
        &binding,
        artifact_resolve::RunArtifactLookup::CanonicalRun { run_id },
        ls_key_opt.as_deref(),
        read_source,
        started,
        uri,
    )
    .await
    .map_err(map_resolve_rpc_err)?;
    crate::spans::mcp_resource_read().in_scope(|| {
        tracing::info!(
            target: "plasm_agent::mcp",
            uri = %uri,
            prompt_hash = %prompt_hash,
            session_id = %session_id,
            run_id = %run_id.to_wire(),
            bytes = resolved.payload.bytes.len(),
            "MCP resources/read"
        );
    });
    crate::metrics::record_mcp_resource_read("canonical", "success", "none", started.elapsed());
    read_run_result(handler, uri, resolved, read_source).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute_session::SessionReuseKey;
    use crate::run_artifacts::{
        document_from_run, ArtifactPayload, ArtifactPayloadMetadata, DocumentFromRun, RunArtifactId,
    };
    use plasm_core::CgsCatalog;

    #[tokio::test]
    async fn computed_artifact_context_is_tsv_with_session_paging_while_ui_is_json() {
        let mut cgs = plasm_core::load_schema(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/python_dag_slice"),
        )
        .unwrap();
        cgs.bind_registry_entry_id("default");
        let cgs = Arc::new(cgs);
        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let logical_id = uuid::Uuid::new_v4();
        let session_ref =
            crate::mcp_logical_ref::format_logical_session_wire_ref_from_uuid(logical_id);
        let host = crate::http::build_plasm_host_state(crate::http::PlasmHostBootstrap {
            engine: plasm_runtime::ExecutionEngine::new(Default::default()).unwrap(),
            mode: plasm_runtime::ExecutionMode::Live,
            registry: Arc::new(plasm_core::discovery::CgsRegistry::from_pairs(vec![(
                "default".into(),
                "Fixture".into(),
                vec![],
                cgs.clone(),
            )])),
            catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        })
        .unwrap();
        let cgs = host
            .catalog
            .snapshot()
            .load_context("default")
            .unwrap()
            .cgs
            .clone();
        let es = crate::test_support::session_fixtures::test_execute_session(
            cgs.clone(),
            &"a".repeat(64),
        );
        let prompt_hash = es.prompt_hash.clone();
        host.sessions
            .insert(
                SessionReuseKey {
                    tenant_scope: String::new(),
                    entry_id: "default".into(),
                    catalog_cgs_hash: cgs.catalog_cgs_hash_hex(),
                    entities: vec!["Pet".into()],
                    context_intent: None,
                    principal: None,
                    logical_session_id: Some(logical_id.to_string()),
                },
                prompt_hash.clone(),
                session_id.clone(),
                es,
            )
            .await;
        host.logical_execute_bindings
            .insert(logical_id, prompt_hash.clone(), session_id.clone())
            .await;
        let step =
            crate::test_support::execution_fixtures::synthetic_published_result_step(32, None);
        let owner = crate::plasm_plan::QualifiedEntityKey {
            entry_id: "default".into(),
            entity: step.entity.unwrap(),
        };
        let preimage =
            plasm_core::expr_parser::ParsedExpr::from_expr(plasm_core::Expr::TeachingValue {
                value: plasm_core::Value::Null,
            });
        let run_id = RunArtifactId::from_wire(&format!("pr{}", "a".repeat(64))).unwrap();
        let doc = document_from_run(DocumentFromRun {
            run_id,
            prompt_hash: &prompt_hash,
            session_id: &session_id,
            qualified_entity: &owner,
            principal: None,
            display_lines: vec!["computed rows".into()],
            parsed_preimage: &preimage,
            result: &step.result,
            resource_index: None,
        })
        .unwrap();
        let payload = ArtifactPayload {
            metadata: ArtifactPayloadMetadata::json_default(),
            bytes: serde_json::to_vec(&doc).unwrap().into(),
        };
        let resolved = artifact_resolve::ResolvedRunArtifact {
            payload: payload.clone(),
            run_id: Some(run_id),
            prompt_hash: prompt_hash.clone(),
            session_id: session_id.clone(),
        };
        let handler = PlasmMcpHandler::new(Arc::new(host));
        let uri = crate::run_artifacts::plasm_run_resource_uri(&prompt_hash, &session_id, &run_id);
        let model = read_run_result(&handler, &uri, resolved.clone(), None)
            .await
            .unwrap();
        let rmcp::model::ResourceContents::TextResourceContents {
            text: model,
            mime_type: model_mime,
            ..
        } = &model.contents[0]
        else {
            panic!("model result must be text");
        };
        assert_eq!(model_mime.as_deref(), Some("text/markdown"));
        assert!(model.contains("```tsv"));
        assert!(
            model.contains(&session_ref),
            "continuation belongs to the resolved logical session"
        );
        assert!(serde_json::from_str::<serde_json::Value>(model).is_err());
        let tool = super::super::resource_helpers::run_artifact_context_payload(
            &handler,
            &prompt_hash,
            &session_id,
            &payload,
        )
        .await
        .unwrap();
        assert_eq!(tool.metadata.content_type, "text/markdown");
        assert!(std::str::from_utf8(&tool.bytes).unwrap().contains("```tsv"));
        let ui = read_run_result(
            &handler,
            &uri,
            resolved,
            Some(plasm_trace::MCP_RESOURCE_READ_SOURCE_RUN_EXPLORER_UI),
        )
        .await
        .unwrap();
        let rmcp::model::ResourceContents::TextResourceContents {
            text: ui,
            mime_type: ui_mime,
            ..
        } = &ui.contents[0]
        else {
            panic!("UI artifact must be JSON text");
        };
        assert_eq!(ui_mime.as_deref(), Some("application/json"));
        let rows: serde_json::Value = serde_json::from_str(ui).unwrap();
        assert_eq!(rows["entities"].as_array().unwrap().len(), 32);
        handler
            .plasm
            .logical_execute_bindings
            .remove(&logical_id)
            .await;
        let error = super::super::resource_helpers::run_artifact_context_payload(
            &handler,
            &prompt_hash,
            &session_id,
            &payload,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("no MCP logical session binding"));
    }
}
