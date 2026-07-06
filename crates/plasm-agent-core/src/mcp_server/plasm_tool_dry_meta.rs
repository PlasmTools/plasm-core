//! Slim agent `_meta.plasm` for dry-run (`plasm` plan-only) tool results.

use plasm_core::PlanCommitRef;
use serde_json::{json, Map, Value};

use super::trace::CodePlanEmitRefs;
use crate::operation::plan_commit_agent_meta;
use crate::PlanDryVerdict;

/// Build agent-facing `_meta.plasm` for a successful dry-run (archive refs live here for UI mirror).
pub(crate) fn build_dry_run_agent_plasm_meta(
    commit_ref: &PlanCommitRef,
    verdict: PlanDryVerdict,
    session_ref: &str,
    plan_refs: &CodePlanEmitRefs,
    domain_revision: u32,
    projection_warning: bool,
) -> Map<String, Value> {
    let mut agent_plasm = plan_commit_agent_meta(commit_ref, verdict);
    agent_plasm.insert("dry_run".into(), Value::Bool(true));
    agent_plasm.insert(
        "logical_session_ref".into(),
        Value::String(session_ref.to_string()),
    );
    // Agent MCP read token: always canonical execute plan URI (never ambiguous `plasm://p/`).
    agent_plasm.insert(
        "plan_uri".into(),
        Value::String(plan_refs.canonical_plan_uri.clone()),
    );
    agent_plasm.insert(
        "plan_http_path".into(),
        Value::String(plan_refs.plan_http_path.clone()),
    );
    agent_plasm.insert(
        "canonical_plan_uri".into(),
        Value::String(plan_refs.canonical_plan_uri.clone()),
    );
    agent_plasm.insert("domain_revision".into(), json!(domain_revision));
    if projection_warning {
        agent_plasm.insert("projection_warning".into(), Value::Bool(true));
    }
    agent_plasm
}
