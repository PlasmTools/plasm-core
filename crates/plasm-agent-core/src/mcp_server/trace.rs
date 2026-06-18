//! MCP trace archive helpers for code-plan evaluate/execute.

use super::*;

struct CodePlanArchiveContext<'a> {
    hub: &'a crate::trace_hub::TraceHub,
    store: &'a crate::run_artifacts::RunArtifactStore,
    mcp_key: &'a str,
    es: &'a ExecuteSession,
    prompt_hash: &'a str,
    session_id: &'a str,
    session_ref: &'a str,
    comp: &'a serde_json::Value,
    program: &'a str,
    comp_summary: serde_json::Value,
    dag_summary: serde_json::Value,
    plan_call_index: u64,
}

enum CodePlanEmitKind<'a> {
    Evaluate,
    Execute { out: &'a PlasmPlanRunResult },
}

#[allow(clippy::too_many_arguments)]
async fn trace_archive_and_emit_code_plan(
    ctx: CodePlanArchiveContext<'_>,
    kind: CodePlanEmitKind<'_>,
) {
    let plan_hash_str = comp_content_sha256_hex(ctx.comp);
    let plan_id = Uuid::new_v4();
    let plan_index = ctx.plan_call_index;
    let handle_str = code_plan_handle(plan_index);
    let doc = CodePlanArchiveDocument {
        kind: "code_plan".into(),
        plan_id: plan_id.to_string(),
        prompt_hash: ctx.prompt_hash.to_string(),
        session_id: ctx.session_id.to_string(),
        entry_id: ctx.es.entry_id.clone(),
        plan_index,
        plan_handle: handle_str.clone(),
        name: plan_display_name_from_comp(ctx.comp),
        code: ctx.program.to_string(),
        plan_hash: plan_hash_str.clone(),
        comp: ctx.comp.clone(),
        catalog_cgs_hash: ctx.es.catalog_cgs_hash.clone(),
        domain_revision: ctx.es.domain_revision,
        entities: ctx.es.entities.clone(),
        principal: ctx.es.principal.clone(),
        created_at: Utc::now().to_rfc3339(),
    };
    let (plan_uri, canonical_plan_uri, plan_http_path) = match ctx
        .store
        .insert_code_plan(ctx.prompt_hash, ctx.session_id, plan_id, plan_index, &doc)
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
                plasm_session_short_plan_uri(ctx.session_ref, plan_index),
                plasm_code_plan_resource_uri(ctx.prompt_hash, ctx.session_id, &plan_id),
                code_plan_http_path(ctx.prompt_hash, ctx.session_id, &plan_id),
            )
        }
    };
    let node_count = plan_node_count_from_comp(ctx.comp);
    let code_chars = ctx.program.chars().count() as u64;
    let trace = match kind {
        CodePlanEmitKind::Evaluate => CodePlanTrace {
            plan_handle: handle_str,
            plan_id: plan_id.to_string(),
            plan_name: plan_display_name_from_comp(ctx.comp),
            plan_hash: plan_hash_str,
            plan_uri,
            canonical_plan_uri,
            plan_http_path,
            prompt_hash: ctx.prompt_hash.to_string(),
            session_id: ctx.session_id.to_string(),
            node_count,
            code_chars,
            comp: ctx.comp_summary,
            dag: ctx.dag_summary,
            plasm_call_index: None,
            run_ids: Vec::new(),
            run_artifacts: Vec::new(),
        },
        CodePlanEmitKind::Execute { out } => {
            let run_ids: Vec<String> = out
                .code_plan_run_artifacts
                .iter()
                .map(|a| a.run_id.clone())
                .collect();
            CodePlanTrace {
                plan_handle: handle_str,
                plan_id: plan_id.to_string(),
                plan_name: plan_display_name_from_comp(ctx.comp),
                plan_hash: plan_hash_str,
                plan_uri,
                canonical_plan_uri,
                plan_http_path,
                prompt_hash: ctx.prompt_hash.to_string(),
                session_id: ctx.session_id.to_string(),
                node_count,
                code_chars,
                comp: ctx.comp_summary,
                dag: ctx.dag_summary,
                plasm_call_index: Some(ctx.plan_call_index),
                run_ids,
                run_artifacts: out.code_plan_run_artifacts.clone(),
            }
        }
    };
    match kind {
        CodePlanEmitKind::Evaluate => {
            ctx.hub
                .trace_record_code_plan_evaluate(ctx.mcp_key, trace)
                .await;
        }
        CodePlanEmitKind::Execute { .. } => {
            ctx.hub
                .trace_record_code_plan_execute(ctx.mcp_key, trace)
                .await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
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
    dag_summary: serde_json::Value,
    plan_call_index: u64,
) {
    trace_archive_and_emit_code_plan(
        CodePlanArchiveContext {
            hub,
            store,
            mcp_key,
            es,
            prompt_hash,
            session_id,
            session_ref,
            comp,
            program,
            comp_summary,
            dag_summary,
            plan_call_index,
        },
        CodePlanEmitKind::Evaluate,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
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
    dag_summary: serde_json::Value,
    plan_call_index: u64,
    out: &PlasmPlanRunResult,
) {
    trace_archive_and_emit_code_plan(
        CodePlanArchiveContext {
            hub,
            store,
            mcp_key,
            es,
            prompt_hash,
            session_id,
            session_ref,
            comp,
            program,
            comp_summary,
            dag_summary,
            plan_call_index,
        },
        CodePlanEmitKind::Execute { out },
    )
    .await;
}
