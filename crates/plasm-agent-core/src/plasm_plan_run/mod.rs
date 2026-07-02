//! Parse, validate, dry-run, and execute Plasm effect [`Plan`](crate::plasm_plan::Plan) programs (HTTP + MCP).

use plasm_core::cgs_federation::FederationDispatch;
use plasm_core::expr_parser::{parse_with_cgs_layers_program, ParseError, ParsedExpr};
use plasm_core::expr_simulation_bindings;
use plasm_core::normalize_expr_query_capabilities;
use plasm_core::normalize_expr_query_capabilities_federated;
use plasm_core::render_intent_with_projection;
use plasm_core::render_intent_with_projection_federated;
use plasm_core::type_check_expr;
use plasm_core::type_check_expr_federated;
use plasm_core::PromptPipelineConfig;
use plasm_core::SymbolMapCrossRequestCache;
use plasm_core::SymbolSession;
use plasm_core::TypeError;
use plasm_core::CGS;

use std::sync::Arc;

use crate::execute_session::ExecuteSession;
use crate::expr_display::expr_display_resolved;
use crate::expr_display::expr_display_resolved_federated;
use crate::http_execute::{
    archive_plasm_result_snapshot, execute_plasm_parsed_expr, trace_record_plasm_line,
    PublishedResultStep,
};
use crate::plan_dry_display;
pub use crate::plan_dry_display::PlanDryReview;
use crate::plasm_plan::{
    AggregateFunction, BindingName, ComputeOp, ComputeTemplate, EffectClass, FieldPath, InputAlias,
    Plan, PlanExprTemplate, PlanNodeId, PlanNodeKind, PlanResultUse, PlanValue, QualifiedEntityKey,
    RelationSourceCardinality, ValidatedForEachNode, ValidatedPlan, ValidatedPlanDataInput,
    ValidatedPlanExprTemplate, ValidatedPlanNode, ValidatedPlanState,
    ValidatedRelationTraversalNode, PLAN_RENDER_MAX_OUTPUT_CHARS, PLAN_RENDER_MAX_ROWS,
};
use crate::server_state::PlasmHostState;
use crate::trace_hub::{CodePlanRunArtifactRef, McpPlasmTraceSink};
use crate::trace_sink_emit::PlasmTraceContext;
use indexmap::IndexMap;
use plasm_core::{
    flatten_from_parent_get_source_rows, EntityName, Expr, Ref, RelationMaterialization,
    RelationRowResolution, TypedFieldValue, Value,
};
use plasm_runtime::{
    entity_to_row_json, CachedEntity, EntityCompleteness, ExecutionResult, ExecutionSource,
    ExecutionStats, MaterializedRowSource,
};
use plasm_trace::TraceCompWire;
use std::collections::{BTreeMap, BTreeSet};

mod compute_eval;
mod dry;
pub mod evidence_plan;
mod materialize;
mod materialize_prefer;
mod orchestrator;
mod parse;
mod plan_bounded_parallel;
mod plan_fanout_parallel;
mod plan_lowering;
mod plan_schedule;
mod prefer_embed_hydrate;
mod relation_hydrate;
mod render_columns;
mod row_json;
mod step_materialize;

#[cfg(all(test, feature = "alloc-bench"))]
mod alloc_bench_test;

pub(crate) use compute_eval::*;
pub(crate) use compute_eval::NodeInputHoleIndex;
pub(crate) use materialize::*;
pub(crate) use relation_hydrate::finalize_typed_relation_materialized_node;
pub(crate) use render_columns::RenderColumns;

pub use dry::{
    evaluate_plasm_comp_dry, node_dependencies, plan_dry_compact_view, render_node_operation,
    render_plasm_plan_dry_text, render_plasm_plan_dry_text_for_session,
};
pub use orchestrator::run_plasm_comp;
pub use parse::{
    dry_run_simulation_for_session, expand_program_surface_for_session_lower,
    format_session_symbolic_parse_error, parse_parsed_expr_for_session,
    parse_plasm_line_for_session, parse_plasm_surface_line, parse_plasm_surface_line_program,
    resolve_wire_field_list, resolve_wire_field_token, session_cgs_layer_stack, session_cgs_layers,
    symbol_map_for_plasm_surface_parse, typecheck_parsed_for_session,
};

#[allow(unused_imports)]
pub(crate) use dry::{
    attach_flow_approval_gates, enrich_graph_summary_auth_scoped_reads, enrich_graph_summary_flow,
    for_each_body_mutates_remote, graph_summary, unused_seed_hints,
};
pub(crate) use orchestrator::{
    inline_row_source, inline_row_source_owned, MaterializedInputRow, MaterializedNode,
};
pub(crate) use parse::{
    entry_scoped_execute_session, propagate_row_identities, row_identities_from_entities,
};
pub(crate) use row_json::{
    cached_entity_row_json, predicate_matches, value_at_dotted, value_at_field_path,
    value_at_segments,
};

#[cfg(test)]
use crate::plasm_plan::{parse_plan_value, validate_plan_artifact};

pub use crate::trace_hub::PlanRunTraceHooks;
pub use plan_lowering::{lowered_ir_digest_from_validated_plan, LoweredIrDigest};

/// Outcome of [`ExecutePipeline::run_program`]: the same `node_results` / optional run payload shape as an MCP
/// live `plasm_run` response (fenced JSON), without Markdown framing.
#[derive(Debug, Clone)]
pub struct PlasmPlanRunResult {
    pub version: serde_json::Value,
    /// One entry per `plan.nodes[]` with `ir`, `simulation`, and optional `id`.
    pub node_results: Vec<serde_json::Value>,
    pub graph_summary: serde_json::Value,
    /// Canonical monadic comp wire for trace/UI (`steps`, `bind`, `return`, …).
    pub comp: Option<TraceCompWire>,
    /// Run snapshots keyed to plan nodes (live execution only).
    pub code_plan_run_artifacts: Vec<CodePlanRunArtifactRef>,
    /// Set when `run` is `true` and the engine returns Markdown (HTTP-backed run path).
    pub run_markdown: Option<String>,
    /// Optional `CallToolResult` `_meta` map (typically includes `plasm` steps when run snapshots exist).
    pub run_plasm_meta: Option<serde_json::Map<String, serde_json::Value>>,
    /// Live execution return roots (for HTTP Accept mapping).
    pub return_steps: Vec<crate::http_execute::PublishedResultStep>,
}

/// Dry-run a program plan: validate, type-check, and render simulation JSON per node.
#[derive(Debug)]
pub struct DryPlasmPlanEvaluation {
    pub version: serde_json::Value,
    pub name: Option<String>,
    artifact: plasm_core::PlasmCompArtifact,
    executable: crate::plasm_comp_lift::ExecutablePlasmComp,
    cached_validated: std::cell::OnceCell<ValidatedPlan>,
    pub topological_order: Vec<String>,
    pub node_results: Vec<serde_json::Value>,
    /// When `true`, every plan node is an independent root surface (no cross-line dependencies);
    /// the host may run those roots in parallel. When `false`, the plan is ordered (DAG/staged nodes).
    pub parallel_root_surfaces_only: bool,
    pub staged_nodes: Vec<String>,
    pub execution_unsupported: Vec<String>,
    pub graph_summary: serde_json::Value,
    pub review: PlanDryReview,
    pub flow: crate::plan_flow::PlanFlowAnalysis,
}

impl DryPlasmPlanEvaluation {
    pub fn validated_plan(&self) -> &Plan<ValidatedPlanState> {
        self.validated().artifact()
    }

    /// Merged flow + boundedness gate (single verdict source for MCP/HTTP).
    pub fn evaluate_gate(&self) -> crate::EvaluatedPlanGate {
        crate::plan_gate::evaluate_plan_gate(
            &self.flow,
            &self.review,
            crate::plan_dry_display::return_roots_include_unbounded_list_surface(
                self.validated_plan(),
            ),
        )
    }

    /// Mint sealed admission for plan commit registration.
    pub fn admit_for_commit(&self) -> Result<crate::FlowAdmission, crate::FlowDenial> {
        crate::plan_flow::FlowCheckedPlan {
            analysis: self.flow.clone(),
        }
        .admit()
    }

    pub fn validated(&self) -> &ValidatedPlan {
        self.cached_validated.get_or_init(|| {
            crate::plan_prepare::build_prepared_validated_plan(
                &self.artifact.comp,
                &self.executable,
            )
            .expect("dry evaluation already validated executable comp")
        })
    }

    /// Detach dry simulation JSON during live execution; restore on the run result.
    pub fn take_node_results_for_live(&mut self) -> Vec<serde_json::Value> {
        std::mem::take(&mut self.node_results)
    }

    pub(crate) fn artifact(&self) -> &plasm_core::PlasmCompArtifact {
        &self.artifact
    }

    /// True when every dry-run node passed preflight (probe-IO gate before `run_ref`).
    #[must_use]
    pub fn probe_preflight_passed(&self) -> bool {
        self.node_results
            .iter()
            .all(|node| node.get("ok").and_then(|v| v.as_bool()).unwrap_or(false))
    }

    /// Rehydrate dry evaluation from a reviewed plan commit (skips simulation when cache is populated).
    pub fn from_plan_commit_cache(
        es: &ExecuteSession,
        bundle: &crate::plasm_comp_bundle::PlasmCompBundle,
        cache: &crate::operation::PlanCommitDryCache,
        review: PlanDryReview,
    ) -> Result<Self, String> {
        let executable = bundle.executable();
        let artifact = bundle.artifact().clone();
        let prepared =
            crate::plan_prepare::build_prepared_validated_plan(&artifact.comp, executable)?;
        let topological_order = if cache.topological_order.is_empty() {
            executable
                .steps_topo
                .iter()
                .map(|(id, _)| id.as_str().to_string())
                .collect()
        } else {
            cache.topological_order.clone()
        };
        let catalog = es.build_flow_catalog_view();
        let flow_checked = crate::plan_flow::verify_plan_flow(
            prepared.artifact(),
            &topological_order,
            &catalog,
            &es.flow_policy,
        );
        let flow_analysis = flow_checked.analysis;
        let mut node_results = cache.node_results.clone();
        attach_flow_approval_gates(&mut node_results, &flow_analysis);
        let mut graph_summary = cache.graph_summary.clone();
        enrich_graph_summary_flow(&mut graph_summary, &flow_analysis);
        Ok(Self {
            version: if cache.version.is_null() {
                serde_json::json!(artifact.comp.version)
            } else {
                cache.version.clone()
            },
            name: cache.name.clone().or_else(|| artifact.comp.name.clone()),
            artifact,
            executable: executable.clone(),
            cached_validated: std::cell::OnceCell::from(prepared),
            topological_order,
            node_results,
            parallel_root_surfaces_only: cache.parallel_root_surfaces_only,
            staged_nodes: cache.staged_nodes.clone(),
            execution_unsupported: cache.execution_unsupported.clone(),
            graph_summary,
            review,
            flow: flow_analysis,
        })
    }
}

/// Optional archive/provenance fields shown at the top of compact dry-run text.
pub struct PlasmPlanDryRunTextMeta<'a> {
    pub plan_name: Option<&'a str>,
    pub plan_handle: &'a str,
    pub plan_uri: &'a str,
    pub canonical_plan_uri: &'a str,
    pub plan_hash: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlasmPlanApprovalDecision {
    Approved,
}

#[derive(Debug, Clone)]
struct PlasmPlanApprovalReceipt {
    decision: PlasmPlanApprovalDecision,
    policy: &'static str,
    gate: serde_json::Value,
}

/// Host-owned approval policy for program-plan write/side-effect nodes.
///
/// The current product default is intentionally automatic so mutating plans can run
/// while the real user/tenant approval surface is built above this boundary.
#[derive(Debug, Clone)]
struct PlasmPlanApprovalPolicy {
    mode: PlasmPlanApprovalMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlasmPlanApprovalMode {
    AutoApprove,
}

impl PlasmPlanApprovalPolicy {
    fn automatic() -> Self {
        Self {
            mode: PlasmPlanApprovalMode::AutoApprove,
        }
    }

    fn review(&self, gate: serde_json::Value) -> PlasmPlanApprovalReceipt {
        match self.mode {
            PlasmPlanApprovalMode::AutoApprove => PlasmPlanApprovalReceipt {
                decision: PlasmPlanApprovalDecision::Approved,
                policy: "host.auto_approve",
                gate,
            },
        }
    }
}

/// Parse, validate, and dry-run a typed plan JSON (test helper; production uses comp bundles).
#[cfg(test)]
pub(crate) fn evaluate_plasm_plan_dry(
    es: &ExecuteSession,
    plan: &serde_json::Value,
) -> Result<DryPlasmPlanEvaluation, String> {
    use crate::plasm_comp_bundle::PlasmCompBundle;
    use crate::plasm_comp_wire::plasm_comp_from_validated;
    use crate::plasm_plan::parse_and_validate_plan_json;
    let validated = parse_and_validate_plan_json(plan)?;
    let artifact = plasm_comp_from_validated(&validated);
    let bundle = PlasmCompBundle::new(artifact)?;
    evaluate_plasm_comp_dry(es, &bundle)
}

fn graph_summary_with_approval_receipts(
    mut graph_summary: serde_json::Value,
    receipts: &[PlasmPlanApprovalReceipt],
) -> serde_json::Value {
    if receipts.is_empty() {
        return graph_summary;
    }
    let receipt_json = receipts
        .iter()
        .map(|r| {
            serde_json::json!({
                "decision": match r.decision {
                    PlasmPlanApprovalDecision::Approved => "approved",
                },
                "policy": r.policy,
                "gate": r.gate,
            })
        })
        .collect::<Vec<_>>();

    if let Some(obj) = graph_summary.as_object_mut() {
        obj.insert(
            "approval_receipts".to_string(),
            serde_json::Value::Array(receipt_json),
        );
    }
    graph_summary
}

#[cfg(test)]
mod tests {
    mod approval_policy;
    mod compute_render;
    mod dry_run;
    mod materialize_tests;
    mod support;
}
