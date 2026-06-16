//! MCP trace archive helpers for code-plan evaluate/execute.

use super::*;

#[allow(clippy::too_many_arguments)] // trace archive carries full execute-session + plan context
pub(crate) async fn trace_archive_and_emit_code_plan_evaluate(
    hub: &crate::trace_hub::TraceHub,
    store: &crate::run_artifacts::RunArtifactStore,
    mcp_key: &str,
    es: &ExecuteSession,
    prompt_hash: &str,
    session_id: &str,
    session_ref: &str,
    comp: &serde_json::Value,
    program: &str,
    comp_summary: serde_json::Value,
    plan_call_index: u64,
) {
    let plan_hash_str = comp_content_sha256_hex(comp);
    let plan_id = Uuid::new_v4();
    let plan_index = plan_call_index;
    let handle_str = code_plan_handle(plan_index);
    let doc = CodePlanArchiveDocument {
        kind: "code_plan".into(),
        plan_id: plan_id.to_string(),
        prompt_hash: prompt_hash.to_string(),
        session_id: session_id.to_string(),
        entry_id: es.entry_id.clone(),
        plan_index,
        plan_handle: handle_str.clone(),
        name: plan_display_name_from_comp(comp),
        code: program.to_string(),
        plan_hash: plan_hash_str.clone(),
        comp: comp.clone(),
        catalog_cgs_hash: es.catalog_cgs_hash.clone(),
        domain_revision: es.domain_revision,
        entities: es.entities.clone(),
        principal: es.principal.clone(),
        created_at: Utc::now().to_rfc3339(),
    };
    let (plan_uri, canonical_plan_uri, plan_http_path) = match store
        .insert_code_plan(prompt_hash, session_id, plan_id, plan_index, &doc)
        .await
    {
        Ok(h) => (h.plasm_uri, h.canonical_plasm_uri, h.http_path),
        Err(e) => {
            tracing::warn!(
                target: "plasm_agent::mcp",
                error = %e,
                "failed to archive Plasm program plan for trace (non-fatal)"
            );
            (
                plasm_session_short_plan_uri(session_ref, plan_index),
                plasm_code_plan_resource_uri(prompt_hash, session_id, &plan_id),
                code_plan_http_path(prompt_hash, session_id, &plan_id),
            )
        }
    };
    let node_count = plan_node_count_from_comp(comp);
    let code_chars = program.chars().count() as u64;
    hub.trace_record_code_plan_evaluate(
        mcp_key,
        CodePlanTrace {
            plan_handle: handle_str,
            plan_id: plan_id.to_string(),
            plan_name: plan_display_name_from_comp(comp),
            plan_hash: plan_hash_str,
            plan_uri,
            canonical_plan_uri,
            plan_http_path,
            prompt_hash: prompt_hash.to_string(),
            session_id: session_id.to_string(),
            node_count,
            code_chars,
            comp: comp_summary,
            plasm_call_index: None,
            run_ids: Vec::new(),
            run_artifacts: Vec::new(),
        },
    )
    .await;
}

#[allow(clippy::too_many_arguments)] // trace archive carries full execute-session + plan context
pub(crate) async fn trace_archive_and_emit_code_plan_execute(
    hub: &crate::trace_hub::TraceHub,
    store: &crate::run_artifacts::RunArtifactStore,
    mcp_key: &str,
    es: &ExecuteSession,
    prompt_hash: &str,
    session_id: &str,
    session_ref: &str,
    comp: &serde_json::Value,
    program: &str,
    comp_summary: serde_json::Value,
    plan_call_index: u64,
    out: &PlasmPlanRunResult,
) {
    let plan_hash_str = comp_content_sha256_hex(comp);
    let plan_id = Uuid::new_v4();
    let plan_index = plan_call_index;
    let handle_str = code_plan_handle(plan_index);
    let doc = CodePlanArchiveDocument {
        kind: "code_plan".into(),
        plan_id: plan_id.to_string(),
        prompt_hash: prompt_hash.to_string(),
        session_id: session_id.to_string(),
        entry_id: es.entry_id.clone(),
        plan_index,
        plan_handle: handle_str.clone(),
        name: plan_display_name_from_comp(comp),
        code: program.to_string(),
        plan_hash: plan_hash_str.clone(),
        comp: comp.clone(),
        catalog_cgs_hash: es.catalog_cgs_hash.clone(),
        domain_revision: es.domain_revision,
        entities: es.entities.clone(),
        principal: es.principal.clone(),
        created_at: Utc::now().to_rfc3339(),
    };
    let (plan_uri, canonical_plan_uri, plan_http_path) = match store
        .insert_code_plan(prompt_hash, session_id, plan_id, plan_index, &doc)
        .await
    {
        Ok(h) => (h.plasm_uri, h.canonical_plasm_uri, h.http_path),
        Err(e) => {
            tracing::warn!(
                target: "plasm_agent::mcp",
                error = %e,
                "failed to archive Plasm program plan for trace (non-fatal)"
            );
            (
                plasm_session_short_plan_uri(session_ref, plan_index),
                plasm_code_plan_resource_uri(prompt_hash, session_id, &plan_id),
                code_plan_http_path(prompt_hash, session_id, &plan_id),
            )
        }
    };
    let node_count = plan_node_count_from_comp(comp);
    let code_chars = program.chars().count() as u64;
    let run_ids: Vec<String> = out
        .code_plan_run_artifacts
        .iter()
        .map(|a| a.run_id.clone())
        .collect();
    hub.trace_record_code_plan_execute(
        mcp_key,
        CodePlanTrace {
            plan_handle: handle_str,
            plan_id: plan_id.to_string(),
            plan_name: plan_display_name_from_comp(comp),
            plan_hash: plan_hash_str,
            plan_uri,
            canonical_plan_uri,
            plan_http_path,
            prompt_hash: prompt_hash.to_string(),
            session_id: session_id.to_string(),
            node_count,
            code_chars,
            comp: comp_summary,
            plasm_call_index: Some(plan_call_index),
            run_ids,
            run_artifacts: out.code_plan_run_artifacts.clone(),
        },
    )
    .await;
}

