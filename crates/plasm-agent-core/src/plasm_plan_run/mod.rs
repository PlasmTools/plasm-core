//! Parse, validate, dry-run, and execute Plasm effect [`Plan`](crate::plasm_plan::Plan) programs (HTTP + MCP).

use plasm_core::cgs_federation::FederationDispatch;
use plasm_core::entity_slices_for_render;
use plasm_core::expr_parser::{parse_with_cgs_layers_program, ParseError, ParsedExpr};
use plasm_core::expr_simulation_bindings;
use plasm_core::normalize_expr_query_capabilities;
use plasm_core::normalize_expr_query_capabilities_federated;
use plasm_core::render_intent_with_projection;
use plasm_core::render_intent_with_projection_federated;
use plasm_core::symbol_map_cache_key_federated;
use plasm_core::symbol_map_cache_key_single_catalog;
use plasm_core::type_check_expr;
use plasm_core::type_check_expr_federated;
use plasm_core::FocusSpec;
use plasm_core::PromptPipelineConfig;
use plasm_core::SymbolMap;
use plasm_core::SymbolMapCrossRequestCache;
use plasm_core::TeachingExposureSession;
use plasm_core::TypeError;
use plasm_core::CGS;

use std::sync::Arc;

use crate::execute_session::ExecuteSession;
use crate::expr_display::expr_display_resolved;
use crate::expr_display::expr_display_resolved_federated;
use crate::http_execute::{
    archive_plasm_result_snapshot, execute_plasm_parsed_expr, publish_plasm_result_steps,
    run_parsed_plasm_line, trace_record_plasm_line, PublishedResultStep,
};
use crate::mcp_plasm_meta::PlasmMetaIndex;
use crate::plan_dry_display;
pub use crate::plan_dry_display::PlanDryReview;
use crate::plasm_plan::{
    AggregateFunction, BindingName, ComputeOp, ComputeTemplate, EffectClass, FieldPath, InputAlias,
    OutputName, Plan, PlanExprTemplate, PlanNodeId, PlanNodeKind, PlanResultUse, PlanValue,
    QualifiedEntityKey, RelationSourceCardinality, ValidatedForEachNode, ValidatedPlan,
    ValidatedPlanDataInput, ValidatedPlanExprTemplate, ValidatedPlanNode, ValidatedPlanState,
    ValidatedRelationTraversalNode, PLAN_RENDER_MAX_OUTPUT_CHARS, PLAN_RENDER_MAX_ROWS,
};
use crate::server_state::PlasmHostState;
use crate::trace_hub::{CodePlanRunArtifactRef, McpPlasmTraceSink};
use crate::trace_sink_emit::PlasmTraceContext;
use indexmap::IndexMap;
use plasm_core::{
    flatten_from_parent_get_source_rows, resolve_relation_row_resolution, EntityName, Expr, Ref,
    RelationMaterialization, RelationRowResolution, TypedFieldValue, Value,
};
use plasm_runtime::{
    entity_to_row_json, CachedEntity, EntityCompleteness, ExecutionResult, ExecutionSource,
    ExecutionStats, MaterializedRowSource,
};
use std::collections::{BTreeMap, BTreeSet};

mod compute_eval;
mod dry;
pub mod evidence_plan;
mod materialize;
mod orchestrator;
mod parse;
mod relation_hydrate;
mod row_json;

pub(crate) use compute_eval::*;
pub(crate) use materialize::*;
pub(crate) use relation_hydrate::finalize_typed_relation_materialized_node;

pub use dry::{
    evaluate_plasm_comp_dry, node_dependencies, plan_dry_compact_view, plan_semantic_dag_json,
    plasm_plan_dag_json, render_node_operation, render_plasm_plan_dry_text,
    render_plasm_plan_dry_text_for_session,
};
pub use orchestrator::run_plasm_comp;
pub use parse::{
    dry_run_simulation_for_session, expand_program_surface_for_session_lower,
    parse_parsed_expr_for_session, parse_plasm_line_for_session, parse_plasm_surface_line,
    parse_plasm_surface_line_program, resolve_wire_field_list, resolve_wire_field_token,
    session_cgs_layers, symbol_map_for_plasm_surface_parse, typecheck_parsed_for_session,
};

pub(crate) use dry::inferred_node_approval;
pub(crate) use orchestrator::{inline_row_source, MaterializedInputRow, MaterializedNode};
pub(crate) use parse::{
    entry_scoped_execute_session, propagate_row_identities, row_identities_from_entities,
};
pub(crate) use row_json::{cached_entity_row_json, predicate_matches, value_at_segments};

#[cfg(test)]
use crate::plasm_plan::{parse_plan_value, validate_plan_artifact};

pub struct PlasmPlanRunHooks<'a> {
    pub meta_index: &'a mut PlasmMetaIndex,
    pub trace: PlasmTraceContext,
    pub sink: McpPlasmTraceSink,
}

/// Outcome of [`ExecutePipeline::run_program`]: the same `node_results` / optional run payload shape as an MCP
/// live `plasm_run` response (fenced JSON), without Markdown framing.
#[derive(Debug, Clone)]
pub struct PlasmPlanRunResult {
    pub version: serde_json::Value,
    /// One entry per `plan.nodes[]` with `ir`, `simulation`, and optional `id`.
    pub node_results: Vec<serde_json::Value>,
    pub graph_summary: serde_json::Value,
    /// Canonical monadic comp wire for trace/UI (`steps`, `bind`, `return`, …).
    pub comp: serde_json::Value,
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
}

impl DryPlasmPlanEvaluation {
    pub fn validated_plan(&self) -> &Plan<ValidatedPlanState> {
        self.validated().artifact()
    }

    pub fn validated(&self) -> &ValidatedPlan {
        self.cached_validated.get_or_init(|| {
            crate::plasm_step_convert::build_validated_plan_from_executable(
                &self.artifact.comp,
                &self.executable,
            )
            .expect("dry evaluation already validated executable comp")
        })
    }

    pub(crate) fn artifact(&self) -> &plasm_core::PlasmCompArtifact {
        &self.artifact
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
    use super::*;
    use plasm_core::load_schema;
    use plasm_core::CgsContext;
    use plasm_core::TeachingExposureSession;
    use std::path::PathBuf;
    use std::sync::Arc;

    #[test]
    fn materialized_result_use_preserves_scalar_data_binding_value() {
        let node = PlanNodeId::new("workspace_id".to_string()).expect("node id");
        let row = serde_json::json!("workspace_123");
        let entities =
            json_rows_to_entities("PlanComputed_workspace_id", std::slice::from_ref(&row));
        let mut materialized = BTreeMap::new();
        materialized.insert(
            node.clone(),
            MaterializedNode {
                entry_id: "acme".to_string(),
                entity: "PlanComputed_workspace_id".to_string(),
                result: ExecutionResult {
                    count: entities.len(),
                    entities: entities.clone(),
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Cache,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 0,
                        cache_hits: 0,
                        cache_misses: 0,
                        ..Default::default()
                    },
                    request_fingerprints: vec![],
                },
                row_source: MaterializedRowSource::Inline(vec![row.clone()]),
                rows: vec![row.clone()],
                row_identities: vec![None],
                artifact: None,
                display: "workspace_id".to_string(),
                projection: None,
            },
        );
        let inputs = materialized_result_use_inputs(
            &materialized,
            &[PlanResultUse {
                node: node.as_str().to_string(),
                r#as: "workspace_id".to_string(),
            }],
        )
        .expect("inputs");
        let alias = InputAlias::new("workspace_id".to_string()).expect("alias");
        assert_eq!(inputs.get(&alias).expect("workspace_id").row, row);
    }

    fn test_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            load_schema(&root.join("tests/fixtures/execute_tiny")).expect("load execute_tiny"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "acme".into(),
            Arc::new(CgsContext::entry("acme", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(cgs.as_ref(), "acme", &["Product", "Category"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "acme".into(),
            String::new(),
            String::new(),
            None,
            vec!["Product".into(), "Category".into()],
            Some(exp),
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    fn duplicate_product_create_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            load_schema(&root.join("tests/fixtures/scoped_create_tiny"))
                .expect("load scoped_create_tiny"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "acme".into(),
            Arc::new(CgsContext::entry("acme", cgs.clone())),
        );
        ctxs.insert(
            "other".into(),
            Arc::new(CgsContext::entry("other", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(cgs.as_ref(), "acme", &["Product"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "acme".into(),
            String::new(),
            String::new(),
            None,
            vec!["Product".into()],
            Some(exp),
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn cmp_json_sort_values_orders_multi_digit_numbers_numerically() {
        use std::cmp::Ordering;
        let n87 = serde_json::json!(87);
        let n300 = serde_json::json!(300);
        assert_eq!(
            cmp_json_sort_values(Some(&n87), Some(&n300)),
            Ordering::Less
        );
        let s87 = serde_json::json!("87");
        let s300 = serde_json::json!("300");
        assert_eq!(
            cmp_json_sort_values(Some(&s87), Some(&s300)),
            Ordering::Less
        );
    }

    #[test]
    fn singleton_input_zero_row_error_is_actionable() {
        let err = singleton_input_row_count_error("src", "_", 0, "staged expression rendering");
        assert!(err.contains("zero rows"), "{err}");
        assert!(err.contains("not a Plasm syntax error"), "{err}");
        assert!(err.contains("branch around empty results"), "{err}");
    }

    #[test]
    fn singleton_input_multi_row_error_mentions_ambiguity_remedy() {
        let err = singleton_input_row_count_error("src", "_", 2, "staged expression rendering");
        assert!(err.contains("2 rows"), "{err}");
        assert!(err.contains("make the source unique"), "{err}");
        assert!(err.contains(".singleton()"), "{err}");
    }

    #[test]
    fn cmp_json_sort_values_string_collates_non_numeric_strings_lexically() {
        use std::cmp::Ordering;
        let apple = serde_json::json!("apple");
        let banana = serde_json::json!("banana");
        assert_eq!(
            cmp_json_sort_values(Some(&apple), Some(&banana)),
            Ordering::Less
        );
    }

    /// Regression: `.sort(score)` must not stringify numbers and compare lexicographically (where
    /// `87` sorts after `300`). Keeps parity with [`eval_compute`] `ComputeOp::Sort` staging.
    #[test]
    fn plan_sort_compute_orders_integer_scores_numerically() {
        let key = FieldPath::from_dotted("score").expect("score path");
        let mut rows = [
            serde_json::json!({"id": "n300", "score": 300}),
            serde_json::json!({"id": "n87", "score": 87}),
            serde_json::json!({"id": "n100", "score": 100}),
        ];
        rows.sort_by(|a, b| cmp_json_sort_values(value_at_path(a, &key), value_at_path(b, &key)));
        assert_eq!(rows[0]["id"], "n87");
        assert_eq!(rows[1]["id"], "n100");
        assert_eq!(rows[2]["id"], "n300");

        rows.reverse();
        assert_eq!(rows[0]["id"], "n300");
        assert_eq!(rows[1]["id"], "n100");
        assert_eq!(rows[2]["id"], "n87");
    }

    fn github_repository_commit_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(load_schema(&root.join("../../apis/github")).expect("load github"));
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "github".into(),
            Arc::new(CgsContext::entry("github", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(cgs.as_ref(), "github", &["Repository", "Commit"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "github".into(),
            String::new(),
            String::new(),
            None,
            vec!["Repository".into(), "Commit".into()],
            Some(exp),
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn entry_scoped_surface_parse_preserves_typed_catalog_create() {
        let s = duplicate_product_create_session();
        let err = parse_parsed_expr_for_session(&s, "Product.create(name=\"bolt\")")
            .expect_err("unscoped federated create should be ambiguous")
            .to_string();
        assert!(err.contains("ambiguous capability label `create`"), "{err}");

        let scoped = entry_scoped_execute_session(
            &s,
            Some(&QualifiedEntityKey {
                entry_id: "other".to_string(),
                entity: "Product".to_string(),
            }),
        )
        .expect("scope session");
        let parsed = parse_parsed_expr_for_session(&scoped, "Product.create(name=\"bolt\")")
            .expect("scoped create parses");
        typecheck_parsed_for_session(&scoped, &parsed).expect("scoped create typechecks");
    }

    #[test]
    fn plan_parses_product_query() {
        let s = test_session();
        let pe = parse_parsed_expr_for_session(&s, "Product").expect("parse");
        let v = serde_json::json!({ "expr": pe.expr, "projection": pe.projection });
        assert!(v.get("expr").is_some());
    }

    /// `e#` is session-local (teaching TSV); single-catalog + exposure must not parse `e1` as an entity *name*.
    /// (`.page_size(n)` is Plasm program postfix sugar; the core line parser does not treat it as Plasm path syntax.)
    #[test]
    fn parse_resolves_e1_with_teaching_exposure() {
        let s = test_session();
        let _ =
            parse_parsed_expr_for_session(&s, "e1").expect("e1 => first taught entity (Product)");
    }

    #[test]
    fn dry_run_typechecks_product_query() {
        let s = test_session();
        let pe = parse_parsed_expr_for_session(&s, "Product").expect("parse");
        typecheck_parsed_for_session(&s, &pe).expect("typecheck");
    }

    #[test]
    fn dry_run_simulation_includes_intent_il_and_bindings() {
        let s = test_session();
        let pe = parse_parsed_expr_for_session(&s, "Product").expect("parse");
        let (intent, il, bindings) = dry_run_simulation_for_session(&s, &pe);
        assert!(
            intent.contains("Query") && intent.contains("Product"),
            "{intent}"
        );
        assert!(il.contains("cap=product_list"), "il must resolve cap: {il}");
        let m = bindings.as_object().expect("object");
        assert_eq!(m.get("op").and_then(|v| v.as_str()), Some("query"));
    }

    #[test]
    fn evaluate_plasm_plan_dry_matches_single_node() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "read-products",
            "nodes": [{
                "id": "n0",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            }],
            "return": { "kind": "node", "node": "n0" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert_eq!(dry.node_results.len(), 1);
        assert!(dry.parallel_root_surfaces_only);
    }

    #[test]
    fn evaluate_plasm_plan_dry_accepts_search_node() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "search-products",
            "nodes": [{
                "id": "search",
                "kind": "search",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product~\"bolt\"",
                "ir": { "expr": { "op": "query", "entity": "Product", "predicate": { "type": "comparison", "field": "q", "op": "=", "value": "bolt" }, "capability_name": "product_search" } },
                "effect_class": "read",
                "result_shape": "list"
            }],
            "return": { "kind": "node", "node": "search" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert!(dry.parallel_root_surfaces_only);
        assert_eq!(dry.node_results[0]["kind"], "search");
    }

    fn langmatrix_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix"))
                .expect("load plasm_language_matrix"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "langmatrix".into(),
            Arc::new(CgsContext::entry("langmatrix", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(cgs.as_ref(), "langmatrix", &["LangItem"]);
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "langmatrix".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangItem".into()],
            Some(exp),
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    fn matrix_views_session() -> ExecuteSession {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix_views"))
                .expect("load plasm_language_matrix_views"),
        );
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "langmatrix_views".into(),
            Arc::new(CgsContext::entry("langmatrix_views", cgs.clone())),
        );
        let exp = TeachingExposureSession::new(
            cgs.as_ref(),
            "langmatrix_views",
            &["LangDigest", "LangItem"],
        );
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs.clone(),
            ctxs,
            "langmatrix_views".into(),
            String::new(),
            String::new(),
            None,
            vec!["LangDigest".into(), "LangItem".into()],
            Some(exp),
            None,
            None,
            cgs.catalog_cgs_hash_hex(),
            None,
            None,
        )
    }

    #[test]
    fn dry_run_rejects_query_node_resolving_search_capability() {
        let s = langmatrix_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "brace-search-mismatch",
            "nodes": [{
                "id": "n0",
                "kind": "query",
                "qualified_entity": { "entry_id": "langmatrix", "entity": "LangItem" },
                "expr": "LangItem{team_key=ENG}",
                "ir": {
                    "expr": {
                        "op": "query",
                        "entity": "LangItem",
                        "predicate": {
                            "type": "comparison",
                            "field": "team_key",
                            "op": "=",
                            "value": "ENG"
                        },
                        "capability_name": "langitem_search"
                    }
                },
                "effect_class": "read",
                "result_shape": "list"
            }],
            "return": { "kind": "node", "node": "n0" }
        });
        let err = evaluate_plasm_plan_dry(&s, &plan).expect_err("query vs search dispatch");
        assert!(
            err.contains("kind query but expression resolved search capability"),
            "{err}"
        );
    }

    #[test]
    fn dry_run_preflight_runs_query_capability_normalize_gates() {
        use crate::execute_pipeline::PlasmPreflight;

        let s = langmatrix_session();
        let pe =
            parse_parsed_expr_for_session(&s, "LangItem{owner=acme}").expect("parse brace owner");
        PlasmPreflight::dry_preview_for_line(&s, "LangItem{owner=acme}", &pe)
            .expect("normalize + compile dispatch for scoped query filter");
    }

    #[test]
    fn dry_run_view_backed_matrix_views_preflight() {
        use crate::execute_pipeline::PlasmPreflight;

        let s = matrix_views_session();
        let lines = [
            r#"LangDigest{item_id="item-1"}"#,
            r#"LangTriageContext{item_id="item-1"}"#,
            r#"LangItemLink{item_id="item-1"}"#,
            r#"LangOwnerFilterDemo{item_id="item-1"}"#,
        ];
        for line in lines {
            let pe = parse_parsed_expr_for_session(&s, line)
                .unwrap_or_else(|err| panic!("parse `{line}`: {err}"));
            PlasmPreflight::dry_preview_for_line(&s, line, &pe).unwrap_or_else(|err| {
                panic!("view-backed dry preflight for `{line}`: {err}");
            });
        }
    }

    #[test]
    fn dry_run_compiled_search_projection_rejects_filter_input_param() {
        use crate::plasm_compile::compile_plasm_expression;
        use plasm_core::PromptPipelineConfig;

        let s = langmatrix_session();
        let pipeline = PromptPipelineConfig::default();
        let source = r#"rows = LangItem~"probe"{team_key="eng"}[q]
rows"#;
        match compile_plasm_expression(&pipeline, None, &s, "search-proj-input", source) {
            Err(err) => {
                assert!(err.contains("is an input on langitem_search"), "{err}");
            }
            Ok(bundle) => {
                let dry_err = evaluate_plasm_comp_dry(&s, &bundle)
                    .expect_err("dry must reject search input projection");
                assert!(
                    dry_err.contains("is an input on langitem_search")
                        || dry_err.contains("postfix projection"),
                    "{dry_err}"
                );
            }
        }
    }

    #[test]
    fn evaluate_plasm_plan_dry_typechecks_ir_not_display_text() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "read-products",
            "nodes": [{
                "id": "n0",
                "kind": "query",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "WrongEntity",
                "ir": { "expr": { "op": "query", "entity": "Product" } },
                "effect_class": "read",
                "result_shape": "list"
            }],
            "return": { "kind": "node", "node": "n0" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert_eq!(
            dry.node_results[0]["execution_contract"]["ir"]["entity"],
            "Product"
        );
    }

    #[test]
    fn evaluate_plasm_plan_dry_rejects_relation_target_mismatch() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "product",
                    "kind": "get",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product(\"p1\")",
                    "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                    "effect_class": "read",
                    "result_shape": "single"
                },
                {
                    "id": "bad_relation",
                    "kind": "relation",
                    "effect_class": "read",
                    "result_shape": "single",
                    "relation": {
                        "source": "product",
                        "relation": "category",
                        "target": { "entry_id": "acme", "entity": "Product" },
                        "cardinality": "one",
                        "source_cardinality": "single",
                        "expr": "Product(\"p1\").category",
                        "ir": { "expr": { "op": "chain", "source": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } }, "selector": "category", "step": { "type": "auto_get" } } }
                    },
                    "depends_on": ["product"],
                    "uses_result": [{ "node": "product", "as": "source" }]
                }
            ],
            "return": { "kind": "node", "node": "bad_relation" }
        });
        let err =
            evaluate_plasm_plan_dry(&s, &plan).expect_err("relation target mismatch rejected");
        assert!(err.contains("does not match CGS target"), "{err}");
    }

    #[test]
    fn evaluate_plasm_plan_dry_typechecks_relation_node() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "product",
                    "kind": "get",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product(\"p1\")",
                    "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                    "effect_class": "read",
                    "result_shape": "single"
                },
                {
                    "id": "category",
                    "kind": "relation",
                    "effect_class": "read",
                    "result_shape": "single",
                    "relation": {
                        "source": "product",
                        "relation": "category",
                        "target": { "entry_id": "acme", "entity": "Category" },
                        "cardinality": "one",
                        "source_cardinality": "single",
                        "expr": "Product(\"p1\").category",
                        "ir": { "expr": { "op": "chain", "source": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } }, "selector": "category", "step": { "type": "auto_get" } } }
                    },
                    "depends_on": ["product"],
                    "uses_result": [{ "node": "product", "as": "source" }]
                }
            ],
            "return": { "kind": "node", "node": "category" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert!(!dry.parallel_root_surfaces_only);
        assert_eq!(
            dry.node_results[1]["simulation"]["kind"],
            "relation_traversal"
        );
    }

    #[test]
    fn evaluate_plasm_plan_dry_accepts_runtime_checked_singleton_relation() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "products",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product",
                    "ir": { "expr": { "op": "query", "entity": "Product" } },
                    "effect_class": "read",
                    "result_shape": "list"
                },
                {
                    "id": "category",
                    "kind": "relation",
                    "effect_class": "read",
                    "result_shape": "single",
                    "relation": {
                        "source": "products",
                        "relation": "category",
                        "target": { "entry_id": "acme", "entity": "Category" },
                        "cardinality": "one",
                        "source_cardinality": "runtime_checked_singleton",
                        "expr": "Product.category",
                        "ir": { "expr": { "op": "chain", "source": { "op": "query", "entity": "Product" }, "selector": "category", "step": { "type": "auto_get" } } }
                    },
                    "depends_on": ["products"],
                    "uses_result": [{ "node": "products", "as": "source" }]
                }
            ],
            "return": { "kind": "node", "node": "category" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert_eq!(
            dry.node_results[1]["simulation"]["kind"],
            "relation_traversal"
        );
    }

    #[test]
    fn evaluate_plasm_plan_dry_accepts_github_relation_limit_aggregate() {
        let s = github_repository_commit_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "github-repo-commits-aggregate",
            "nodes": [
                {
                    "id": "repo",
                    "kind": "get",
                    "qualified_entity": { "entry_id": "github", "entity": "Repository" },
                    "expr": "Repository({owner=\"ryan-s-roberts\", repo=\"plasm-core\"})",
                    "ir": { "expr": { "op": "get", "ref": { "entity_type": "Repository", "key": { "owner": "ryan-s-roberts", "repo": "plasm-core" } } } },
                    "effect_class": "read",
                    "result_shape": "single"
                },
                {
                    "id": "commits",
                    "kind": "relation",
                    "effect_class": "read",
                    "result_shape": "list",
                    "relation": {
                        "source": "repo",
                        "relation": "commits",
                        "target": { "entry_id": "github", "entity": "Commit" },
                        "cardinality": "many",
                        "source_cardinality": "single",
                        "expr": "Repository({owner=\"ryan-s-roberts\", repo=\"plasm-core\"}).commits[sha,message]",
                        "ir": { "expr": { "op": "chain", "source": { "op": "get", "ref": { "entity_type": "Repository", "key": { "owner": "ryan-s-roberts", "repo": "plasm-core" } } }, "selector": "commits", "step": { "type": "auto_get" } }, "projection": ["sha", "message"] }
                    },
                    "qualified_entity": { "entry_id": "github", "entity": "Commit" },
                    "projection": ["sha", "message"],
                    "depends_on": ["repo"],
                    "uses_result": [{ "node": "repo", "as": "source" }]
                },
                {
                    "id": "limited",
                    "kind": "compute",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "compute": {
                        "source": "commits",
                        "op": { "kind": "limit", "count": 2000 },
                        "schema": {
                            "entity": "PlanLimit",
                            "fields": [
                                { "name": "sha", "value_kind": "string", "source": ["sha"] },
                                { "name": "message", "value_kind": "string", "source": ["message"] }
                            ]
                        }
                    },
                    "depends_on": ["commits"]
                },
                {
                    "id": "n_commits",
                    "kind": "compute",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "compute": {
                        "source": "limited",
                        "op": { "kind": "aggregate", "aggregates": [{ "name": "n", "function": "count" }] },
                        "schema": { "entity": "PlanAggregate", "fields": [{ "name": "n", "value_kind": "integer" }] }
                    },
                    "depends_on": ["limited"]
                }
            ],
            "return": { "kind": "node", "node": "n_commits" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert_eq!(
            dry.node_results[1]["simulation"]["kind"],
            "relation_traversal"
        );
        assert_eq!(dry.node_results[2]["kind"], "compute");
        assert_eq!(dry.node_results[3]["kind"], "compute");
    }

    #[test]
    fn dry_run_text_renders_dependency_dag_snapshot() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "product-summary",
            "nodes": [
                {
                    "id": "products",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product",
                    "ir": { "expr": { "op": "query", "entity": "Product" } },
                    "effect_class": "read",
                    "result_shape": "list"
                },
                {
                    "id": "summary",
                    "kind": "compute",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "compute": {
                        "source": "products",
                        "op": { "kind": "project", "fields": { "sku": ["id"], "name": ["name"] } },
                        "schema": {
                            "entity": "PlanProject",
                            "fields": [
                                { "name": "sku", "value_kind": "unknown", "source": ["id"] },
                                { "name": "name", "value_kind": "unknown", "source": ["name"] }
                            ]
                        }
                    }
                },
                {
                    "id": "cards",
                    "kind": "derive",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "derive_template": {
                        "kind": "map",
                        "source": "summary",
                        "item_binding": "product",
                        "value": {
                            "kind": "object",
                            "fields": {
                                "title": { "kind": "template", "template": "${product.name}", "input_bindings": [{ "from": "product.name", "to": "" }] }
                            }
                        }
                    },
                    "depends_on": ["summary"],
                    "uses_result": [{ "node": "summary", "as": "product" }]
                }
            ],
            "return": { "kind": "parallel", "nodes": ["summary", "cards"] }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        let comp = crate::plasm_comp_wire::plasm_comp_json_from_dry(&dry);
        assert!(comp.get("steps").and_then(|s| s.get("products")).is_some());
        assert_eq!(comp["bind"]["deps"]["summary"][0], "products");
        assert_eq!(comp["bind"]["deps"]["cards"][0], "summary");
        let text = render_plasm_plan_dry_text(
            &dry,
            Some(PlasmPlanDryRunTextMeta {
                plan_name: None,
                plan_handle: "p7",
                plan_uri: "plasm://session/s0/p/7",
                canonical_plan_uri: "plasm://execute/ph/s/plan/uuid",
                plan_hash: "abc123",
            }),
        );
        insta::assert_snapshot!(
            text,
            @"
        plan review · 3n 1r → parallel(2) · p7
        warn: project list reads; unused seed acme:Category; unbounded read

        01 products     query Query(Product all)
        02 summary      project name, sku ← products
        03 cards        derive map summary as product → {1} ← summary
        "
        );
        assert!(!text.contains("node_results"));
        assert!(!text.contains("\"dry_run\""));
    }

    #[test]
    fn scoped_node_symbols_evaluate_against_singleton_inputs() {
        let value = PlanValue::Object {
            fields: BTreeMap::from([
                (
                    "title".to_string(),
                    PlanValue::Template {
                        template: "${p.name} uses ${moveFacts.move}".to_string(),
                        input_bindings: vec![],
                    },
                ),
                (
                    "power".to_string(),
                    PlanValue::NodeSymbol {
                        node: "moveFacts".to_string(),
                        alias: "moveFacts".to_string(),
                        path: vec!["power".to_string()],
                    },
                ),
            ]),
        };
        let row = serde_json::json!({ "name": "pikachu" });
        let inputs = BTreeMap::from([(
            InputAlias::new("moveFacts".to_string()).expect("alias"),
            MaterializedInputRow {
                node: PlanNodeId::new("moveFacts".to_string()).expect("node id"),
                proof: crate::plasm_plan::InputCardinalityProof::StaticSingleton,
                row: serde_json::json!({ "move": "thunderbolt", "power": 90 }),
                row_identity: None,
            },
        )]);
        let binding = BindingName::new("p".to_string()).expect("binding");
        let scope = EvalScope::Bound {
            row: &row,
            binding: &binding,
        };
        let input_env = InputEnv { rows: &inputs };
        let env = PlanEvalEnv {
            scope,
            inputs: input_env,
            wire_coercion: None,
        };
        let out = eval_plan_value(&value, &env).expect("eval");
        assert_eq!(out["title"], "pikachu uses thunderbolt");
        assert_eq!(out["power"], 90);
    }

    #[test]
    fn render_compute_emits_single_content_row() {
        let rows = vec![
            serde_json::json!({ "name": "a" }),
            serde_json::json!({ "name": "b" }),
        ];
        let columns = vec![OutputName::new("name").expect("column")];
        let out = render_compute(
            &rows,
            &columns,
            "{% for r in rows %}- {{ r.name }}\n{% endfor %}",
        )
        .expect("render");

        assert_eq!(out, vec![serde_json::json!({ "content": "- a\n- b\n" })]);
    }

    #[test]
    fn render_compute_propagates_minijinja_errors() {
        let rows = vec![serde_json::json!({ "name": "a" })];
        let columns = vec![OutputName::new("name").expect("column")];
        let err = render_compute(&rows, &columns, "{{ missing }}")
            .expect_err("strict undefined is rejected");

        assert!(err.contains("Plan.render template render error"), "{err}");
    }

    #[test]
    fn render_compute_rejects_missing_columns() {
        let rows = vec![serde_json::json!({ "name": "a" })];
        let columns = vec![OutputName::new("missing").expect("column")];
        let err =
            render_compute(&rows, &columns, "{{ rows }}").expect_err("missing column rejected");

        assert!(err.contains("did not resolve in source row 0"), "{err}");
    }

    #[test]
    fn render_compute_preserves_unicode_markdown() {
        let rows = vec![serde_json::json!({
            "title": "Pokémon",
            "arrow": "→",
        })];
        let columns = vec![
            OutputName::new("title").expect("title"),
            OutputName::new("arrow").expect("arrow"),
        ];
        let rendered = render_compute(
            &rows,
            &columns,
            "# {{ rows[0].title }}\nstep {{ rows[0].arrow }} done",
        )
        .expect("render unicode");
        let content = rendered[0]["content"].as_str().expect("content");
        assert!(content.contains("Pokémon"), "{content}");
        assert!(content.contains('→'), "{content}");
    }

    #[test]
    fn render_compute_feeds_node_input_for_action_content() {
        let rows = vec![
            serde_json::json!({ "name": "a" }),
            serde_json::json!({ "name": "b" }),
        ];
        let columns = vec![OutputName::new("name").expect("column")];
        let rendered = render_compute(
            &rows,
            &columns,
            "{% for r in rows %}- {{ r.name }}\n{% endfor %}",
        )
        .expect("render");
        let input = rendered.into_iter().next().expect("singleton row");
        let value = PlanValue::Object {
            fields: BTreeMap::from([(
                "content".to_string(),
                PlanValue::NodeSymbol {
                    node: "doc".to_string(),
                    alias: "doc".to_string(),
                    path: vec!["content".to_string()],
                },
            )]),
        };
        let inputs = BTreeMap::from([(
            InputAlias::new("doc".to_string()).expect("alias"),
            MaterializedInputRow {
                node: PlanNodeId::new("doc".to_string()).expect("node id"),
                proof: crate::plasm_plan::InputCardinalityProof::StaticSingleton,
                row: input,
                row_identity: None,
            },
        )]);
        let scope = EvalScope::Root {
            row: &serde_json::Value::Null,
        };
        let env = PlanEvalEnv {
            scope,
            inputs: InputEnv { rows: &inputs },
            wire_coercion: None,
        };
        let out = eval_plan_value(&value, &env).expect("eval");

        assert_eq!(out["content"], "- a\n- b\n");
    }

    #[test]
    fn validation_rejects_ambiguous_auto_cross_node_input() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [
                {
                    "id": "products",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product",
                    "ir": { "expr": { "op": "query", "entity": "Product" } },
                    "effect_class": "read",
                    "result_shape": "list"
                },
                {
                    "id": "tags",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "data": { "kind": "literal", "value": [{ "tag": "a" }] }
                },
                {
                    "id": "cards",
                    "kind": "derive",
                    "effect_class": "artifact_read",
                    "result_shape": "artifact",
                    "derive_template": {
                        "kind": "map",
                        "source": "tags",
                        "item_binding": "tag",
                        "inputs": [{ "node": "products", "alias": "products", "cardinality": "auto" }],
                        "value": { "kind": "node_symbol", "node": "products", "alias": "products", "path": ["name"] }
                    }
                }
            ],
            "return": { "kind": "node", "node": "cards" }
        });
        let err = crate::plasm_plan::validate_plan_value(&plan).expect_err("ambiguous input");
        assert!(err.contains("not statically singleton"), "{err}");
    }

    #[test]
    fn evaluate_plasm_plan_dry_reports_for_each_stage() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "label-products",
            "nodes": [
                {
                    "id": "find",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product",
                    "ir": { "expr": { "op": "query", "entity": "Product" } },
                    "effect_class": "read",
                    "result_shape": "list"
                },
                {
                    "id": "label",
                    "kind": "for_each",
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack",
                    "source": "find",
                    "item_binding": "product",
                    "depends_on": ["find"],
                    "uses_result": [{ "node": "find", "as": "product" }],
                    "approval": "label_products",
                    "effect_template": {
                        "kind": "action",
                        "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                        "expr_template": "Product(${product.id}).label(label=\"stale\")",
                        "ir_template": {
                            "expr": {
                                "op": "invoke",
                                "capability": "product_label",
                                "target": { "entity_type": "Product", "key": { "__plasm_hole": { "kind": "binding", "binding": "product", "path": ["id"] } } },
                                "input": { "label": "stale" }
                            },
                            "input_bindings": [{ "from": "product.id", "to": "id" }]
                        },
                        "effect_class": "side_effect",
                        "result_shape": "side_effect_ack"
                    }
                }
            ],
            "return": { "kind": "parallel", "nodes": ["find", "label"] }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert!(!dry.parallel_root_surfaces_only);
        assert_eq!(dry.node_results.len(), 2);
        assert_eq!(dry.node_results[1]["simulation"]["kind"], "template_stage");
        assert_eq!(
            dry.node_results[1]["approval_gate"]["policy_key"],
            "acme.Product.label"
        );
    }

    #[test]
    fn for_each_templates_render_concrete_row_bound_plasm_calls() {
        let plan = parse_plan_value(&serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "label-products",
            "nodes": [
                {
                    "id": "find",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "data": { "kind": "literal", "value": [{ "id": "p1" }] }
                },
                {
                    "id": "label",
                    "kind": "for_each",
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack",
                    "source": "find",
                    "item_binding": "product",
                    "depends_on": ["find"],
                    "uses_result": [{ "node": "find", "as": "product" }],
                    "effect_template": {
                        "kind": "action",
                        "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                        "expr_template": "Product(${product.id}).label(label=\"stale\")",
                        "ir_template": {
                            "expr": {
                                "op": "invoke",
                                "capability": "product_label",
                                "target": { "entity_type": "Product", "key": { "__plasm_hole": { "kind": "binding", "binding": "product", "path": ["id"] } } },
                                "input": { "label": "stale" }
                            },
                            "input_bindings": [{ "from": "product.id", "to": "id" }]
                        },
                        "effect_class": "side_effect",
                        "result_shape": "side_effect_ack"
                    }
                }
            ],
            "return": { "kind": "node", "node": "label" }
        }))
        .expect("parse plan");
        let validated = validate_plan_artifact(&plan).expect("validate plan");
        let for_each = validated
            .nodes()
            .iter()
            .find_map(|node| match node {
                ValidatedPlanNode::ForEach(node) => Some(node),
                _ => None,
            })
            .expect("for_each node");
        let expressions = render_for_each_expressions(
            for_each,
            &[serde_json::json!({ "id": "p1", "name": "Bolt" })],
            None,
        )
        .expect("render expressions");

        assert_eq!(
            expressions,
            vec!["Product(\"p1\").label(label=\"stale\")".to_string()]
        );
    }

    #[test]
    fn dry_run_text_includes_review_for_unbounded_list_root() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "unbounded-products",
            "nodes": [
                {
                    "id": "products",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product",
                    "ir": { "expr": { "op": "query", "entity": "Product" } },
                    "effect_class": "read",
                    "result_shape": "list"
                }
            ],
            "return": { "kind": "node", "node": "products" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        let text = render_plasm_plan_dry_text(&dry, None);
        assert!(text.contains("plan review"), "{text}");
        assert!(text.contains("warn:"), "{text}");
        assert!(text.contains("unbounded read"), "{text}");
        assert!(text.contains("project list reads"), "{text}");
        assert!(dry.review.has_unbounded_read_root, "{:?}", dry.review);
    }

    #[test]
    fn dry_run_text_review_omits_broad_list_nudge_for_bounded_single_get() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "one-product",
            "nodes": [
                {
                    "id": "one",
                    "kind": "get",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product(\"p1\")",
                    "ir": { "expr": { "op": "get", "ref": { "entity_type": "Product", "key": "p1" } } },
                    "effect_class": "read",
                    "result_shape": "single"
                }
            ],
            "return": { "kind": "node", "node": "one" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        let text = render_plasm_plan_dry_text(&dry, None);
        assert!(text.starts_with("plan review"), "{text}");
        assert!(
            !text.contains("unbounded read"),
            "bounded get should not get unbounded risk: {text}"
        );
        assert!(text.contains("unused seed acme:Category"), "{text}");
    }

    #[test]
    fn dry_run_text_renders_staged_read_map_body() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "dependent-product-read",
            "nodes": [
                {
                    "id": "products",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product",
                    "ir": { "expr": { "op": "query", "entity": "Product" } },
                    "effect_class": "read",
                    "result_shape": "list"
                },
                {
                    "id": "details",
                    "kind": "for_each",
                    "effect_class": "read",
                    "result_shape": "single",
                    "source": "products",
                    "item_binding": "product",
                    "depends_on": ["products"],
                    "uses_result": [{ "node": "products", "as": "product" }],
                    "effect_template": {
                        "kind": "get",
                        "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                        "expr_template": "Product(${product.id})",
                        "ir_template": {
                            "expr": {
                                "op": "get",
                                "ref": {
                                    "entity_type": "Product",
                                    "key": { "__plasm_hole": { "kind": "binding", "binding": "product", "path": ["id"] } }
                                }
                            },
                            "input_bindings": [{ "from": "product.id", "to": "id" }]
                        },
                        "effect_class": "read",
                        "result_shape": "single"
                    }
                }
            ],
            "return": { "kind": "node", "node": "details" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert_eq!(dry.staged_nodes, vec!["details (ForEach)"]);
        assert!(dry.execution_unsupported.is_empty());
        let text = render_plasm_plan_dry_text(&dry, None);
        assert!(
            text.contains("for_each products as product => Product(${product.id})"),
            "{text}"
        );
        assert!(!text.contains("=> {}"), "{text}");
        assert!(
            text.contains("plan review"),
            "root list read is unbounded in this fixture: {text}"
        );
    }

    #[test]
    fn dry_run_text_renders_empty_literals_explicitly() {
        let plan = parse_plan_value(&serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "empty",
                "kind": "data",
                "effect_class": "artifact_read",
                "result_shape": "artifact",
                "data": { "kind": "object", "fields": {} }
            }],
            "return": { "kind": "node", "node": "empty" }
        }))
        .expect("parse plan");
        let validated = validate_plan_artifact(&plan).expect("validate");
        let operation = render_node_operation(&validated.nodes()[0]);
        assert_eq!(operation, "data {0 fields}");
        assert!(!operation.contains("{}"), "{operation}");
    }

    #[test]
    fn create_template_approval_uses_create_operation_not_description_text() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "create-report",
            "nodes": [
                {
                    "id": "products",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product",
                    "ir": { "expr": { "op": "query", "entity": "Product" } },
                    "effect_class": "read",
                    "result_shape": "list"
                },
                {
                    "id": "createIssue",
                    "kind": "for_each",
                    "effect_class": "write",
                    "result_shape": "mutation_result",
                    "source": "products",
                    "item_binding": "product",
                    "depends_on": ["products"],
                    "uses_result": [{ "node": "products", "as": "product" }],
                    "effect_template": {
                        "kind": "create",
                        "qualified_entity": { "entry_id": "linear", "entity": "Issue" },
                        "expr_template": "Issue.create(title=\"Report\", description=\"1.) text that looks like member syntax\")",
                        "ir_template": {
                            "expr": {
                                "op": "create",
                                "capability": "issue_create",
                                "entity": "Issue",
                                "input": {
                                    "title": "Report",
                                    "description": "1.) text that looks like member syntax"
                                }
                            },
                            "input_bindings": []
                        },
                        "effect_class": "write",
                        "result_shape": "mutation_result"
                    }
                }
            ],
            "return": { "kind": "node", "node": "createIssue" }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert_eq!(
            dry.graph_summary["approval_gates"][0]["policy_key"],
            "linear.Issue.create"
        );
        let text = render_plasm_plan_dry_text(&dry, None);
        assert!(
            !text.contains("approval:"),
            "dry-run text omits approval policy lines (auto-approved host policy): {text}"
        );
    }

    #[test]
    fn mutating_for_each_infers_approval_without_agent_label() {
        let s = test_session();
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "label-products",
            "nodes": [
                {
                    "id": "find",
                    "kind": "query",
                    "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                    "expr": "Product",
                    "ir": { "expr": { "op": "query", "entity": "Product" } },
                    "effect_class": "read",
                    "result_shape": "list"
                },
                {
                    "id": "label",
                    "kind": "for_each",
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack",
                    "source": "find",
                    "item_binding": "product",
                    "depends_on": ["find"],
                    "uses_result": [{ "node": "find", "as": "product" }],
                    "effect_template": {
                        "kind": "action",
                        "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                        "expr_template": "Product(${product.id}).label(label=\"stale\")",
                        "ir_template": {
                            "expr": {
                                "op": "invoke",
                                "capability": "product_label",
                                "target": { "entity_type": "Product", "key": { "__plasm_hole": { "kind": "binding", "binding": "product", "path": ["id"] } } },
                                "input": { "label": "stale" }
                            },
                            "input_bindings": [{ "from": "product.id", "to": "id" }]
                        },
                        "effect_class": "side_effect",
                        "result_shape": "side_effect_ack"
                    }
                }
            ],
            "return": { "kind": "parallel", "nodes": ["find", "label"] }
        });
        let dry = evaluate_plasm_plan_dry(&s, &plan).expect("dry");
        assert_eq!(
            dry.graph_summary["approval_gates"][0]["policy_key"],
            "acme.Product.label"
        );
    }

    #[test]
    fn mutating_surface_gate_declares_default_auto_approval() {
        let plan = serde_json::json!({
            "version": 1,
            "kind": "program",
            "nodes": [{
                "id": "c1",
                "kind": "create",
                "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                "expr": "Product.create(name=\"servo\")",
                "ir": { "expr": { "op": "create", "capability": "product_create", "entity": "Product", "input": { "name": "servo" } } },
                "effect_class": "write",
                "result_shape": "single"
            }],
            "return": { "kind": "node", "node": "c1" }
        });
        let typed = parse_plan_value(&plan).expect("parse plan");
        let validated = validate_plan_artifact(&typed).expect("validate");
        let gate = inferred_node_approval(&validated.nodes()[0]).expect("approval gate");

        assert_eq!(gate["policy_key"], "acme.Product.create");
        assert_eq!(gate["host_policy"], "host.auto_approve");
        assert_eq!(gate["default_decision"], "approved");
    }

    #[test]
    fn automatic_approval_policy_emits_receipt_for_gate() {
        let gate = serde_json::json!({
            "node": "c1",
            "required": true,
            "policy_key": "acme.Product.create"
        });
        let receipt = PlasmPlanApprovalPolicy::automatic().review(gate.clone());
        let summary = graph_summary_with_approval_receipts(serde_json::json!({}), &[receipt]);

        assert_eq!(summary["approval_receipts"][0]["decision"], "approved");
        assert_eq!(
            summary["approval_receipts"][0]["policy"],
            "host.auto_approve"
        );
        assert_eq!(summary["approval_receipts"][0]["gate"], gate);
    }

    #[test]
    fn for_each_plan_eval_env_interpolates_row_and_cross_binding_strings() {
        let row = serde_json::json!({"title": "Bolt"});
        let mut input_rows = BTreeMap::new();
        input_rows.insert(
            InputAlias::new("report".to_string()).expect("alias"),
            MaterializedInputRow {
                node: PlanNodeId::new("report").expect("node"),
                proof: crate::plasm_plan::InputCardinalityProof::StaticSingleton,
                row: serde_json::json!({"content": "STATS"}),
                row_identity: None,
            },
        );
        let binding = BindingName::new("_".to_string()).expect("binding");
        let scope = EvalScope::Bound {
            row: &row,
            binding: &binding,
        };
        let inputs = InputEnv { rows: &input_rows };
        let env = PlanEvalEnv {
            scope,
            inputs,
            wire_coercion: None,
        };
        let out = instantiate_expr_template_value(
            &serde_json::json!("${_.title} / ${report.content}"),
            &env,
        )
        .expect("interpolate");
        assert_eq!(out, serde_json::json!("Bolt / STATS"));
    }

    #[test]
    fn for_each_cross_uses_materialization_wires_upstream_singleton() {
        let plan = parse_plan_value(&serde_json::json!({
            "version": 1,
            "kind": "program",
            "name": "cross-binding-for-each",
            "nodes": [
                {
                    "id": "find",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "list",
                    "data": { "kind": "literal", "value": [{ "id": "p1", "title": "Bolt" }] }
                },
                {
                    "id": "report",
                    "kind": "data",
                    "effect_class": "artifact_read",
                    "result_shape": "single",
                    "data": { "kind": "literal", "value": { "content": "STATS" } }
                },
                {
                    "id": "label",
                    "kind": "for_each",
                    "effect_class": "side_effect",
                    "result_shape": "side_effect_ack",
                    "source": "find",
                    "item_binding": "_",
                    "depends_on": ["find", "report"],
                    "uses_result": [
                        { "node": "find", "as": "_" },
                        { "node": "report", "as": "report" }
                    ],
                    "effect_template": {
                        "kind": "action",
                        "qualified_entity": { "entry_id": "acme", "entity": "Product" },
                        "expr_template": "Product.create(title=<<T\n${_.title} ${report.content}\nT\n)",
                        "ir_template": {
                            "expr": {
                                "op": "create",
                                "capability": "product_create",
                                "entity": "Product",
                                "input": { "title": "<<T\n${_.title} ${report.content}\nT\n" }
                            },
                            "input_bindings": []
                        },
                        "effect_class": "side_effect",
                        "result_shape": "side_effect_ack"
                    }
                }
            ],
            "return": { "kind": "node", "node": "label" }
        }))
        .expect("parse");
        let validated = validate_plan_artifact(&plan).expect("validate");
        let for_each = validated
            .nodes()
            .iter()
            .find_map(|node| match node {
                ValidatedPlanNode::ForEach(node) => Some(node),
                _ => None,
            })
            .expect("for_each");
        let mut materialized = BTreeMap::new();
        materialized.insert(
            PlanNodeId::new("report").expect("report"),
            MaterializedNode {
                entry_id: "acme".into(),
                entity: "Report".into(),
                result: ExecutionResult {
                    entities: vec![],
                    count: 1,
                    has_more: false,
                    pagination_resume: None,
                    paging_handle: None,
                    source: ExecutionSource::Cache,
                    stats: ExecutionStats {
                        duration_ms: 0,
                        network_requests: 0,
                        cache_hits: 0,
                        cache_misses: 0,
                        ..Default::default()
                    },
                    request_fingerprints: vec![],
                },
                row_source: MaterializedRowSource::Inline(vec![
                    serde_json::json!({"content": "STATS"}),
                ]),
                rows: vec![serde_json::json!({"content": "STATS"})],
                row_identities: vec![None],
                artifact: None,
                display: String::new(),
                projection: None,
            },
        );
        let input_rows =
            materialized_result_use_inputs(&materialized, &for_each_cross_uses(for_each))
                .expect("input rows");
        assert_eq!(input_rows.len(), 1);
        let row = serde_json::json!({"id": "p1", "title": "Bolt"});
        let env = for_each_plan_eval_env(for_each, &row, &input_rows);
        let out = instantiate_expr_template_value(
            &serde_json::json!("${_.title} ${report.content}"),
            &env,
        )
        .expect("interpolate");
        assert_eq!(out, serde_json::json!("Bolt STATS"));
    }

    fn test_host_state() -> crate::server_state::PlasmHostState {
        use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
        use plasm_core::discovery::InMemoryCgsRegistry;
        use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
        use std::sync::Arc;

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = Arc::new(
            load_schema(&root.join("tests/fixtures/execute_tiny")).expect("load execute_tiny"),
        );
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "acme".into(),
            "Acme".into(),
            vec!["demo".into()],
            cgs.clone(),
        )]);
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(reg),
            catalog_bootstrap: crate::server_state::CatalogBootstrap::Fixed,
            plugin_manager: None,
            incoming_auth: None,
            run_artifacts: Arc::new(crate::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        })
    }

    #[tokio::test]
    async fn from_parent_get_materializes_embedded_relation_refs() {
        use crate::plasm_plan::{
            PlanNodeId, RelationCardinality, RelationName, ResultShape, ValidatedPlanExprIr,
            ValidatedPlanNode, ValidatedPlanRelationTraversal, ValidatedRelationTraversalNode,
        };
        use crate::plasm_plan_run::orchestrator::MaterializedNode;
        use plasm_core::{Expr, GetExpr, JsonPathSegment, RelationMaterialization, Value};

        let st = test_host_state();
        let es = test_session();
        let parent_ref = Ref::new("Product", "p1");
        let cat_ref = Ref::new("Category", "c1");

        let mut cat = CachedEntity::new(cat_ref.clone(), 1);
        cat.fields.insert(
            "id".into(),
            TypedFieldValue::from(Value::String("c1".into())),
        );
        cat.fields.insert(
            "name".into(),
            TypedFieldValue::from(Value::String("Tools".into())),
        );

        let mut parent = CachedEntity::new(parent_ref.clone(), 1);
        parent
            .relations
            .insert("category".into(), vec![cat_ref.clone()]);

        {
            let mut g = es.lock_graph_cache().await;
            g.insert(cat).expect("insert cat");
            g.insert(parent.clone()).expect("insert parent");
        }

        let parent_row = parent.payload_to_json();
        let source_mat = MaterializedNode {
            entry_id: "acme".into(),
            entity: "Product".into(),
            result: ExecutionResult {
                count: 1,
                entities: vec![parent.clone()],
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats::default(),
                request_fingerprints: vec![],
            },
            row_source: MaterializedRowSource::Inline(vec![parent_row.clone()]),
            rows: vec![parent_row],
            row_identities: vec![None],
            artifact: None,
            display: r#"Product("p1")"#.into(),
            projection: None,
        };

        let product_id = PlanNodeId::new("product").expect("product id");
        let category_id = PlanNodeId::new("category").expect("category id");
        let relation_wrap = ValidatedRelationTraversalNode {
            id: category_id,
            effect_class: EffectClass::Read,
            result_shape: ResultShape::Single,
            relation: ValidatedPlanRelationTraversal {
                source: product_id.clone(),
                relation: RelationName::new("category").expect("relation"),
                target: QualifiedEntityKey {
                    entry_id: "acme".into(),
                    entity: "Category".into(),
                },
                cardinality: RelationCardinality::One,
                source_cardinality: RelationSourceCardinality::RuntimeCheckedSingleton,
                ir: ValidatedPlanExprIr {
                    expr: Expr::Get(GetExpr::new("Category", "c1")),
                    projection: None,
                    display_expr: Some(r#"Product("p1").category"#.into()),
                },
                materialize: RelationMaterialization::FromParentGet {
                    path: vec![JsonPathSegment::Key {
                        key: "category".into(),
                    }],
                },
                binding_proofs: vec![],
            },
            depends_on: vec![product_id],
            uses_result: vec![PlanResultUse {
                node: "product".into(),
                r#as: "product".into(),
            }],
        };
        let relation_node = ValidatedPlanNode::RelationTraversal(relation_wrap);

        let mat = try_materialize_from_parent_get_relation(
            &st,
            &es,
            "test-session",
            &relation_node,
            match &relation_node {
                ValidatedPlanNode::RelationTraversal(n) => n,
                _ => panic!("relation node"),
            },
            &source_mat,
            &source_mat.rows,
            None,
        )
        .await
        .expect("materialize")
        .expect("some rows");

        assert_eq!(mat.result.count, 1, "embedded category ref must resolve");
        assert_eq!(mat.result.entities[0].reference, cat_ref);
    }
}
