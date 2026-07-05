//! MCP `resources/read` for Plasm run snapshots and MCP App UI bundles.

use std::sync::Arc;
use std::time::Instant;

use rust_mcp_sdk::schema::{
    ReadResourceContent, ReadResourceRequestParams, ReadResourceResult, RpcError,
};
use rust_mcp_sdk::McpServer;

use crate::run_artifacts::{
    parse_plasm_execute_plan_uri, parse_plasm_session_short_plan_uri,
    strip_plasm_resource_read_source,
};

use super::artifact_resolve;
use super::discover::read_resource_result_for_payload;
use super::resource_read_trace;
use super::transport::PlasmExecBinding;
use super::PlasmMcpHandler;

async fn resolve_session_binding(
    handler: &PlasmMcpHandler,
    runtime: &Arc<dyn McpServer>,
    uri: &str,
    read_source: Option<&str>,
    started: Instant,
) -> Result<(PlasmExecBinding, String), RpcError> {
    let logical_uuid =
        artifact_resolve::logical_uuid_from_session_scoped_uri(uri).map_err(map_resolve_rpc_err)?;
    let ls_key = logical_uuid.to_string();
    let transport_key = runtime.session_id();
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
        return Err(RpcError::invalid_params().with_message(
            "no execute session for this logical session: call plasm_context with capability picks (`seeds`) first",
        ));
    };
    Ok((b, ls_key))
}

async fn resolve_session_scoped_run(
    handler: &PlasmMcpHandler,
    runtime: &Arc<dyn McpServer>,
    uri: &str,
    read_source: Option<&str>,
    started: Instant,
) -> Result<artifact_resolve::ResolvedRunArtifact, RpcError> {
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
    runtime: &Arc<dyn McpServer>,
    uri: &str,
    read_source: Option<&str>,
    started: Instant,
) -> Result<artifact_resolve::ResolvedCodePlanArtifact, RpcError> {
    let (_, plan_index) = parse_plasm_session_short_plan_uri(uri).ok_or_else(|| {
        RpcError::invalid_params().with_message(format!("unsupported resource URI: {uri}"))
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

fn map_resolve_rpc_err(e: artifact_resolve::RunArtifactResolveError) -> RpcError {
    match e {
        artifact_resolve::RunArtifactResolveError::DecodeFailed(msg) => {
            RpcError::internal_error().with_message(format!("run artifact decode failed: {msg}"))
        }
        artifact_resolve::RunArtifactResolveError::Integrity(msg) => RpcError::internal_error()
            .with_message(format!("run artifact integrity check failed: {msg}")),
        other => RpcError::invalid_params().with_message(other.to_string()),
    }
}

pub(crate) async fn handle_read_resource_request(
    handler: &PlasmMcpHandler,
    params: ReadResourceRequestParams,
    runtime: Arc<dyn McpServer>,
) -> Result<ReadResourceResult, RpcError> {
    let started = Instant::now();
    let raw_uri = params.uri.trim();
    let (uri_owned, read_source) = strip_plasm_resource_read_source(raw_uri);
    let uri = uri_owned.as_str();
    let read_source = read_source.as_deref();
    if let Some(bundle) = crate::mcp_app::bundle_for_uri(uri) {
        let Some((content, result_meta)) = crate::mcp_app::read_resource_text(uri) else {
            return Err(
                RpcError::invalid_params().with_message(format!("unknown ui resource: {uri}"))
            );
        };
        crate::metrics::record_mcp_resource_read(
            bundle.resource_metric,
            "success",
            "none",
            started.elapsed(),
        );
        return Ok(ReadResourceResult {
            contents: vec![ReadResourceContent::TextResourceContents(content)],
            meta: Some(result_meta),
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
        return read_resource_result_for_payload(
            uri,
            crate::run_artifacts::project_artifact_payload_for_mcp_read(
                &resolved.payload,
                read_source,
            )
            .map_err(|e| {
                RpcError::internal_error()
                    .with_message(format!("run artifact projection failed: {e}"))
            })?,
        );
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
        return Err(
            RpcError::invalid_params().with_message(format!("unsupported resource URI: {uri}"))
        );
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
    read_resource_result_for_payload(
        uri,
        crate::run_artifacts::project_artifact_payload_for_mcp_read(&resolved.payload, read_source)
            .map_err(|e| {
                RpcError::internal_error()
                    .with_message(format!("run artifact projection failed: {e}"))
            })?,
    )
}
