//! Hermit-backed matrix: parse → DAG compile → dry validate → live plan run for Plasm programs.
//!
//! This file is the conformance surface for user-visible Plasm syntax against the dedicated
//! `plasm_language_matrix` OpenAPI + CGS fixtures.
//!
//! ## Coverage contract (keep in sync when extending the language)
//!
//! Each [`MatrixRow`] should exercise a **distinct** user-visible construct or sugar called out in
//! [`docs/plasm-language-definition.md`](../../../../docs/plasm-language-definition.md):
//!
//! - Entity roots: bare query, search `~`, get `(id)`, brace predicates `{field=value}`, comparisons.
//! - Postfix: `.limit`, `.sort(field[, dir])` including `asc`/`desc`, `.aggregate` (named + sugar),
//!   `.group_by`, `.singleton()`, `.page_size`, bracket projection `[…]`.
//! - Programs: bindings, node-ref continuation, parallel final roots, `compile_plasm_expression`
//!   (single-line surface) vs multi-line DAG programs.
//! - Relations: `from_parent_get`, `query_scoped`, opaque `r#` nav (not `p#`), one-cardinality `r#`,
//!   homograph `p#` forgiven when LHS binding label matches relation wire.
//! - Programs: flattened single-liner coercion (space-separated bindings; first binding default return);
//!   same path via `compile_plasm_expression` when DAG-shaped.
//! - Render: bracket render `<<TAG`, and passing **`.content`** into a typed string slot (`create`).
//! - Effects: create / update / delete / zero-arity action (domain-stripped method label), `for_each`.
//! - teaching table: `e#` symbols where applicable.
//!
//! Hermit returns **schema-generated** bodies; live assertions target stable row payloads, not
//! OpenAPI `example` literals. **Live `run_markdown`** is fenced TSV for row-shaped HTTP results
//! ([`mcp_format_execute_result_table_or_tsv`](../../plasm-agent-core/src/mcp_run_markdown.rs));
//! operation display strings (`Query(…)`, `Get(…)`) are asserted on dry-run IR in [`assert_planning_ir`].
//! Multi-digit **numeric** `.sort` ordering is covered in
//! `plasm-agent-core` (`plan_sort_compute_orders_integer_scores_numerically`) because Hermit list
//! payloads are not example-stable.
//!
//! **Planning:** dry-run [`DryPlasmPlanEvaluation::node_results`] `ir.expr` JSON is deserialized into
//! typed [`plasm_core::Expr`]; compute stages deserialize into [`plasm_agent::plasm_plan::ComputeOp`].
//! We avoid matching rendered `operation` strings. Where the IR omits host-only flags (for example
//! surface [`page_size`](plasm_agent::plasm_plan::PlanNode::page_size)), we assert structured fields
//! on the compiled plan JSON rather than human-readable plan text.

#[path = "common/hermit_lang_matrix.rs"]
mod hermit_lang_matrix;

#[path = "common/language_matrix.rs"]
mod language_matrix;

use std::collections::BTreeSet;

use plasm_agent::execute_session::ExecuteSession;
use plasm_agent::plasm_compile::{compile_plasm_expression, compile_plasm_program};
use plasm_agent::plasm_plan::{AggregateFunction, ComputeOp, ComputeTemplate, PlanValue};
use plasm_agent::plasm_plan_run::{
    evaluate_plasm_comp_dry, run_plasm_comp, DryPlasmPlanEvaluation, PlasmPlanRunResult,
};
use plasm_core::{
    ChainStep, CompOp, EntityKey, Expr, GetExpr, InvokeExpr, Predicate, PromptPipelineConfig,
    QueryExpr, TypedComparisonValue, Value,
};
use plasm_agent::server_state::PlasmHostState;
use plasm_runtime::{ExecutionConfig, ExecutionEngine};

/// Every tag listed here must appear on at least one passing [`MATRIX_ROWS`] entry (`features` column).
const REQUIRED_FEATURE_TAGS: &[&str] = &[
    "entity_query",
    "entity_search",
    "entity_get",
    "predicate_brace_equality",
    "predicate_brace_comparison",
    "postfix_limit",
    "postfix_projection",
    "postfix_sort",
    "postfix_sort_ascending",
    "postfix_aggregate",
    "aggregate_sugar_count",
    "aggregate_sum",
    "postfix_singleton",
    "relation_from_parent_get",
    "relation_query_scoped",
    "bindings_assignment",
    "bind_first_postfix_limit",
    "binding_continuation",
    "bind_limit1_continuation",
    "bind_projection_then_relation",
    "bind_relation_hop_one_one",
    "parallel_final_roots",
    "bracket_render",
    "bracket_render_content_ref",
    "static_heredoc_binding",
    "derive_map",
    "effect_create",
    "effect_update",
    "effect_delete",
    "effect_action",
    "for_each_effect",
    "domain_symbol_e1",
    "postfix_group_by",
    "postfix_group_by_aggregate_chain",
    "postfix_row_filter",
    "postfix_group_by_sugar",
    "postfix_group_by_multi",
    "federated_relation_target_entry",
    "federated_duplicate_entity_symbol",
    "federated_duplicate_entity_relation_r",
    "federated_duplicate_entity_mutator_m",
    "federated_parallel_roots",
    "federated_group_by_on_e1",
    "bracket_render_inline_on_e",
    "pagination_page_size",
    "pagination_fetch_all_default",
    "surface_line_compile",
    "relation_many_from_plural",
    "relation_prefer_from_parent_get",
    "relation_prefer_embed_hit",
    "relation_prefer_embed_miss",
    "relation_query_scoped_bindings",
    "relation_binding_proof",
    "binding_opaque_relation_ref",
    "relation_opaque_r_symbol",
    "relation_one_opaque_r",
    "homograph_lhs_coercion",
    "flattened_single_liner_coercion",
    "flattened_surface_line_compile",
    "postfix_group_by_sort",
    "search_then_group_by",
    "postfix_dedupe",
    "agg_first_last",
    "dry_live_parity",
    "utf8_dollar_interpolate",
    "host_wait_cancel",
    "monadic_comp_witness",
];

struct MatrixRow {
    id: &'static str,
    program: &'static str,
    /// Use [`compile_plasm_expression`] for this row (single expression / comma roots).
    surface_line: bool,
    /// Federated session: primary `linear` + secondary `pokeapi` (same matrix CGS, distinct `Arc`).
    federated: bool,
    features: &'static [&'static str],
    /// Minimum [`PlasmPlanRunResult::node_results`] length after live run.
    min_node_results: usize,
    /// Each substring must appear in [`PlasmPlanRunResult::run_markdown`].
    expect_markdown_substrings: &'static [&'static str],
}

fn assert_comp_witness(dry: &DryPlasmPlanEvaluation) -> Result<(), String> {
    use plasm_agent::{plasm_comp_from_validated, trace_comp_wire_from_dry};
    let artifact = plasm_comp_from_validated(dry.validated());
    artifact.comp.validate().map_err(|e| e.to_string())?;
    let wire = trace_comp_wire_from_dry(dry);
    if wire.comp.steps.is_empty() {
        return Err("comp wire steps must be non-empty".into());
    }
    if wire.comp.bind.topo.is_empty() {
        return Err("comp wire bind.topo must be non-empty".into());
    }
    Ok(())
}

fn surface_exprs(dry: &DryPlasmPlanEvaluation) -> Vec<Expr> {
    dry.node_results
        .iter()
        .filter_map(|nr| {
            let ev = nr.get("ir")?.get("expr")?;
            serde_json::from_value(ev.clone()).ok()
        })
        .collect()
}

fn relation_exprs(dry: &DryPlasmPlanEvaluation) -> Vec<Expr> {
    dry.node_results
        .iter()
        .filter_map(|nr| {
            let ev = nr.get("execution_contract")?.get("ir")?;
            serde_json::from_value(ev.clone()).ok()
        })
        .collect()
}

fn compute_templates(dry: &DryPlasmPlanEvaluation) -> Vec<ComputeTemplate> {
    dry.node_results
        .iter()
        .filter_map(|nr| {
            nr.get("compute")
                .and_then(|c| serde_json::from_value::<ComputeTemplate>(c.clone()).ok())
        })
        .collect()
}

fn tcv_string(v: &TypedComparisonValue) -> Option<String> {
    match v.to_value() {
        Value::String(s) => Some(s),
        Value::Integer(n) => Some(n.to_string()),
        _ => None,
    }
}

fn json_contains_selector_field(v: &serde_json::Value, want: &str) -> bool {
    match v {
        serde_json::Value::Object(map) => {
            if map.get("selector").and_then(|s| s.as_str()) == Some(want) {
                return true;
            }
            map.values().any(|x| json_contains_selector_field(x, want))
        }
        serde_json::Value::Array(a) => a.iter().any(|x| json_contains_selector_field(x, want)),
        _ => false,
    }
}

fn comp_has_relation_named(comp: &serde_json::Value, relation: &str) -> bool {
    comp_relation_named(comp, relation).is_some()
}

fn comp_relation_named<'a>(
    comp: &'a serde_json::Value,
    relation: &str,
) -> Option<&'a serde_json::Value> {
    let steps = comp.get("steps")?.as_object()?;
    for step in steps.values() {
        if step.get("kind").and_then(|k| k.as_str()) == Some("flat_map_relation")
            && step.pointer("/relation/relation").and_then(|x| x.as_str()) == Some(relation)
        {
            return step.get("relation");
        }
    }
    None
}

fn comp_ir_contains_selector(comp: &serde_json::Value, want: &str) -> bool {
    let Some(steps) = comp.get("steps").and_then(|s| s.as_object()) else {
        return false;
    };
    steps.values().any(|n| {
        [n.get("ir"), n.get("ir_template")]
            .into_iter()
            .flatten()
            .any(|ir| json_contains_selector_field(ir, want))
    })
}

fn json_value_contains_substring(v: &serde_json::Value, needle: &str) -> bool {
    match v {
        serde_json::Value::String(s) => s.contains(needle),
        serde_json::Value::Array(a) => a.iter().any(|x| json_value_contains_substring(x, needle)),
        serde_json::Value::Object(o) => {
            o.values().any(|x| json_value_contains_substring(x, needle))
        }
        _ => false,
    }
}

fn tcv_integer(v: &TypedComparisonValue) -> Option<i64> {
    match v.to_value() {
        Value::Integer(n) => Some(n),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn first_query(exprs: &[Expr]) -> Result<&QueryExpr, String> {
    for e in exprs {
        if let Expr::Query(q) = e {
            return Ok(q);
        }
    }
    Err("expected a Query IR node".into())
}

fn comp_surface_page_size(comp: &serde_json::Value) -> Option<u64> {
    let steps = comp.get("steps")?.as_object()?;
    for step in steps.values() {
        if step.get("kind").and_then(|k| k.as_str()) == Some("invoke") {
            return step.get("page_size")?.as_u64();
        }
    }
    None
}

fn comp_steps_values(comp: &serde_json::Value) -> Vec<&serde_json::Value> {
    comp.get("steps")
        .and_then(|s| s.as_object())
        .map(|m| m.values().collect())
        .unwrap_or_default()
}

fn comp_first_invoke_qualified_entity(comp: &serde_json::Value) -> Option<&serde_json::Value> {
    comp_steps_values(comp)
        .into_iter()
        .find(|s| s.get("kind").and_then(|k| k.as_str()) == Some("invoke"))
        .and_then(|s| s.get("qualified_entity"))
}

fn comp_has_invoke_plan_kind(comp: &serde_json::Value, plan_kind: &str) -> bool {
    comp_steps_values(comp).iter().any(|n| {
        n.get("kind").and_then(|k| k.as_str()) == Some("invoke")
            && n.get("plan_kind").and_then(|k| k.as_str()) == Some(plan_kind)
    })
}

#[allow(clippy::too_many_lines)]
fn assert_planning_ir(
    row: &MatrixRow,
    dry: &DryPlasmPlanEvaluation,
    comp: &serde_json::Value,
) -> Result<(), String> {
    let surfaces = surface_exprs(dry);
    let computes = compute_templates(dry);
    let rel = relation_exprs(dry);

    match row.id {
        "lang_query_all" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!("expected LangItem query, got {:?}", q.entity));
            }
            if q.predicate.is_some() {
                return Err(format!(
                    "expected unpredicated query, got {:?}",
                    q.predicate
                ));
            }
            if q.capability_name.as_deref() != Some("langitem_query") {
                return Err(format!(
                    "expected explicit langitem_query capability, got {:?}",
                    q.capability_name
                ));
            }
            if !computes.is_empty() {
                return Err(format!(
                    "expected no compute stages, got {}",
                    computes.len()
                ));
            }
        }
        "lang_surface_line_limit" | "lang_bind_first_limit" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" || q.predicate.is_some() {
                return Err(format!("unexpected query IR: {q:?}"));
            }
            let want = if row.id == "lang_surface_line_limit" {
                2usize
            } else {
                3usize
            };
            let Some(ComputeOp::Limit { count }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|o| matches!(o, ComputeOp::Limit { .. }))
            else {
                return Err(format!("expected Limit compute, got {:?}", computes));
            };
            if *count != want {
                return Err(format!("expected limit {want}, got {count}"));
            }
        }
        "lang_search" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!("expected LangItem, got {:?}", q.entity));
            }
            let Some(cap) = q.capability_name.as_ref() else {
                return Err("search query should pin a Search capability".into());
            };
            if cap.as_str() != "langitem_search" {
                return Err(format!("expected langitem_search capability, got {cap}"));
            }
            let Some(pred) = q.predicate.as_ref() else {
                return Err("expected search predicate".into());
            };
            let Predicate::Comparison {
                field,
                op: CompOp::Eq,
                value,
            } = pred
            else {
                return Err(format!("expected equality predicate, got {pred:?}"));
            };
            if field != "q" {
                return Err(format!("expected search field q, got {field}"));
            }
            if tcv_string(value).as_deref() != Some("Alpha") {
                return Err(format!(
                    "expected Alpha search text, got {:?}",
                    tcv_string(value)
                ));
            }
        }
        "lang_get_by_id" => {
            if !surfaces
                .iter()
                .any(|e| expr_contains_get_langitem(e, Some("i1")))
            {
                return Err(format!(
                    "expected LangItem(i1) Get IR, got {:?}",
                    surfaces.first()
                ));
            }
        }
        "lang_predicate_brace_owner" => {
            let q = first_query(&surfaces)?;
            // `capability_name` may be inferred later in the pipeline; brace IR stability is the predicate.
            let Some(pred) = q.predicate.as_ref() else {
                return Err("expected owner predicate".into());
            };
            let Predicate::Comparison {
                field,
                op: CompOp::Eq,
                value,
            } = pred
            else {
                return Err(format!("expected owner eq, got {pred:?}"));
            };
            if field != "owner" || tcv_string(value).as_deref() != Some("alice") {
                return Err(format!("unexpected predicate: {pred:?}"));
            }
        }
        "lang_predicate_brace_score_cmp" => {
            let q = first_query(&surfaces)?;
            if q.capability_name.as_ref().map(|c| c.as_str()) == Some("langitem_query_owner") {
                return Err("score comparison must not route to langitem_query_owner".into());
            }
            let Some(pred) = q.predicate.as_ref() else {
                return Err("expected comparison predicate".into());
            };
            let Predicate::Comparison {
                field,
                op: CompOp::Gt,
                value,
            } = pred
            else {
                return Err(format!("expected score gt, got {pred:?}"));
            };
            if field != "score" || tcv_integer(value) != Some(1) {
                return Err(format!("unexpected predicate: {pred:?}"));
            }
        }
        "lang_limit_projection" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!("expected LangItem, got {:?}", q.entity));
            }
            let Some(ComputeOp::Limit { count: 1 }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|o| matches!(o, ComputeOp::Limit { .. }))
            else {
                return Err(format!("expected Limit(1), got {:?}", computes));
            };
        }
        "lang_sort_limit" => {
            let Some(ComputeOp::Sort {
                descending: true, ..
            }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|o| matches!(o, ComputeOp::Sort { .. }))
            else {
                return Err(format!("expected descending Sort, got {:?}", computes));
            };
            let Some(ComputeOp::Limit { count: 2 }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|o| matches!(o, ComputeOp::Limit { .. }))
            else {
                return Err(format!("expected Limit(2), got {:?}", computes));
            };
        }
        "lang_sort_asc" => {
            let Some(ComputeOp::Sort {
                descending: false, ..
            }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|o| matches!(o, ComputeOp::Sort { .. }))
            else {
                return Err(format!("expected ascending Sort, got {:?}", computes));
            };
            let Some(ComputeOp::Limit { count: 3 }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|o| matches!(o, ComputeOp::Limit { .. }))
            else {
                return Err(format!("expected Limit(3), got {:?}", computes));
            };
        }
        "lang_aggregate" => {
            let Some(ComputeTemplate {
                op: ComputeOp::Aggregate { aggregates },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::Aggregate { .. }))
            else {
                return Err(format!("expected Aggregate compute, got {:?}", computes));
            };
            let Some(spec) = aggregates.iter().find(|a| a.name.as_str() == "n") else {
                return Err(format!(
                    "expected aggregate binding n, got {:?}",
                    aggregates
                ));
            };
            if spec.function != AggregateFunction::Count || spec.field.is_some() {
                return Err(format!("unexpected aggregate spec: {spec:?}"));
            }
        }
        "lang_aggregate_sugar_count" => {
            let Some(ComputeTemplate {
                op: ComputeOp::Aggregate { aggregates },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::Aggregate { .. }))
            else {
                return Err(format!("expected Aggregate compute, got {:?}", computes));
            };
            let Some(spec) = aggregates.iter().find(|a| a.name.as_str() == "count") else {
                return Err(format!(
                    "expected sugar binding count, got {:?}",
                    aggregates
                ));
            };
            if spec.function != AggregateFunction::Count || spec.field.is_some() {
                return Err(format!("unexpected aggregate spec: {spec:?}"));
            }
        }
        "lang_aggregate_sum" => {
            let Some(ComputeTemplate {
                op: ComputeOp::Aggregate { aggregates },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::Aggregate { .. }))
            else {
                return Err(format!("expected Aggregate compute, got {:?}", computes));
            };
            let Some(spec) = aggregates.iter().find(|a| a.name.as_str() == "t") else {
                return Err(format!(
                    "expected aggregate binding t, got {:?}",
                    aggregates
                ));
            };
            if spec.function != AggregateFunction::Sum {
                return Err(format!("expected sum, got {:?}", spec.function));
            }
            if spec.field.as_ref().is_none_or(|p| p.dotted() != "score") {
                return Err(format!("expected sum(score), got {:?}", spec.field));
            }
        }
        "lang_group_by_sugar" => {
            let Some(ComputeTemplate {
                op: ComputeOp::GroupBy { keys, aggregates },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::GroupBy { .. }))
            else {
                return Err(format!("expected GroupBy compute, got {:?}", computes));
            };
            if keys.len() != 1 || keys[0].dotted() != "owner" {
                return Err(format!("expected key owner, got {:?}", keys));
            }
            let Some(spec) = aggregates.iter().find(|a| a.name.as_str() == "count") else {
                return Err(format!("expected count=count sugar, got {:?}", aggregates));
            };
            if spec.function != AggregateFunction::Count {
                return Err(format!("unexpected aggregate: {spec:?}"));
            }
        }
        "lang_group_by_multi" => {
            let Some(ComputeTemplate {
                op: ComputeOp::GroupBy { keys, aggregates },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::GroupBy { .. }))
            else {
                return Err(format!("expected GroupBy compute, got {:?}", computes));
            };
            if keys.len() != 2 {
                return Err(format!("expected two group keys, got {:?}", keys));
            }
            if keys[0].dotted() != "owner" || keys[1].dotted() != "score" {
                return Err(format!("expected owner+score keys, got {:?}", keys));
            }
            if !aggregates.iter().any(|a| a.name.as_str() == "n") {
                return Err(format!("expected aggregate n, got {:?}", aggregates));
            }
        }
        "lang_row_filter_brace" | "lang_row_filter_paren" => {
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Filter { .. }))
            {
                return Err(format!("expected Filter compute, got {:?}", computes));
            }
        }
        "lang_group_by" => {
            let Some(ComputeTemplate {
                op: ComputeOp::GroupBy { keys, aggregates },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::GroupBy { .. }))
            else {
                return Err(format!("expected GroupBy compute, got {:?}", computes));
            };
            if keys.len() != 1 || keys[0].dotted() != "owner" {
                return Err(format!("expected group key owner, got {:?}", keys));
            }
            let Some(spec) = aggregates.iter().find(|a| a.name.as_str() == "n") else {
                return Err(format!("expected aggregate n, got {:?}", aggregates));
            };
            if spec.function != AggregateFunction::Count {
                return Err(format!("unexpected aggregate: {spec:?}"));
            }
        }
        "lang_group_by_aggregate_chain" => {
            let Some(ComputeTemplate {
                op: ComputeOp::GroupBy { keys, aggregates },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::GroupBy { .. }))
            else {
                return Err(format!("expected GroupBy compute, got {:?}", computes));
            };
            if keys.len() != 2 {
                return Err(format!("expected two group keys, got {:?}", keys));
            }
            if keys[0].dotted() != "owner" || keys[1].dotted() != "score" {
                return Err(format!("expected owner+score keys, got {:?}", keys));
            }
            if !aggregates.iter().any(|a| a.name.as_str() == "n") {
                return Err(format!("expected aggregate n, got {:?}", aggregates));
            }
            if !aggregates.iter().any(|a| a.name.as_str() == "title") {
                return Err(format!("expected aggregate title, got {:?}", aggregates));
            }
        }
        "lang_search_then_group_by" => {
            let Some(ComputeTemplate {
                op: ComputeOp::GroupBy { keys, .. },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::GroupBy { .. }))
            else {
                return Err(format!("expected GroupBy after search, got {:?}", computes));
            };
            if keys.len() != 1 || keys[0].dotted() != "owner" {
                return Err(format!(
                    "expected group key owner on search rows, got {:?}",
                    keys
                ));
            }
            let q = first_query(&surfaces)?;
            if q.capability_name.as_deref() != Some("langitem_search") {
                return Err(format!(
                    "expected langitem_search upstream, got {:?}",
                    q.capability_name
                ));
            }
        }
        "lang_search_then_group_by_team_key" => {
            let Some(ComputeTemplate {
                op: ComputeOp::GroupBy { keys, .. },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::GroupBy { .. }))
            else {
                return Err(format!("expected GroupBy after search, got {:?}", computes));
            };
            if keys.len() != 1 || keys[0].dotted() != "team_key" {
                return Err(format!(
                    "expected group key team_key on search rows, got {:?}",
                    keys
                ));
            }
            let q = first_query(&surfaces)?;
            if q.capability_name.as_deref() != Some("langitem_search") {
                return Err(format!(
                    "expected langitem_search upstream, got {:?}",
                    q.capability_name
                ));
            }
        }
        "lang_relation_lines" => {
            if !surfaces
                .iter()
                .any(|e| expr_contains_get_langitem(e, Some("i1")))
            {
                return Err(format!(
                    "expected LangItem(i1) in surface IR (possibly under Chain), got {:?}",
                    surfaces
                ));
            }
            let pool: Vec<&Expr> = surfaces.iter().chain(rel.iter()).collect();
            // `from_parent_get` often lowers through `.lines` chain navigation; LangLine may appear in
            // the explicit continuation rather than as a bare `Query { entity: LangLine }` root.
            if !pool
                .iter()
                .copied()
                .any(|e| expr_chain_selects_lines(e) || expr_mentions_langline(e))
            {
                return Err(format!(
                    "expected `.lines` chain and/or LangLine IR, got surfaces={surfaces:?} rel={rel:?}"
                ));
            }
        }
        "lang_query_singleton" => {
            let Some(ComputeOp::Limit { count: 5 }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|o| matches!(o, ComputeOp::Limit { .. }))
            else {
                return Err(format!("expected Limit(5), got {:?}", computes));
            };
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" || q.predicate.is_some() {
                return Err(format!(
                    "expected bare LangItem query before singleton tail, got {q:?}"
                ));
            }
            // `.singleton()` is primarily a runtime cardinality proof + relation constraint; it does not
            // reliably surface as `result_shape: single` on serialized plan nodes for every lowering.
        }
        "lang_relation_tags_scoped" => {
            let tags_ir = surfaces
                .iter()
                .chain(rel.iter())
                .any(expr_chain_selects_tags);
            if !tags_ir {
                return Err(format!(
                    "expected `.tags` relation chain IR, got surfaces={surfaces:?} rel={rel:?}",
                ));
            }
            let rel_plan = comp_relation_named(comp, "tags")
                .ok_or_else(|| "expected `.tags` relation on LangItem(i1).tags".to_string())?;
            if rel_plan
                .pointer("/materialize/kind")
                .and_then(|k| k.as_str())
                != Some("prefer_from_parent_get")
            {
                return Err(format!(
                    "expected prefer_from_parent_get on scoped tags row, got {rel_plan:?}"
                ));
            }
        }
        "lang_bindings_render" => {
            let Some(ComputeTemplate {
                op: ComputeOp::Render { .. },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::Render { .. }))
            else {
                return Err(format!("expected Render compute, got {:?}", computes));
            };
        }
        "lang_render_content_into_create" => {
            let has_create_node = comp_has_invoke_plan_kind(comp, "create");
            if !has_create_node {
                return Err(format!(
                    "expected a comp invoke `create` step (Create may be staged with `ir_template`, not dry `ir.expr`), got {:?}",
                    comp.get("steps")
                ));
            }
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Render { .. }))
            {
                return Err("expected bracket Render compute before create".into());
            }
        }
        "lang_utf8_minijinja_dollar_stitch" => {
            let has_create_node = comp_has_invoke_plan_kind(comp, "create");
            if !has_create_node {
                return Err(format!(
                    "expected a comp invoke `create` step, got {:?}",
                    comp.get("steps")
                ));
            }
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Render { .. }))
            {
                return Err("expected bracket Render compute before create".into());
            }
            let mut saw_utf8 = false;
            for nr in &dry.node_results {
                if json_value_contains_substring(nr, "Pokémon") {
                    saw_utf8 = true;
                    break;
                }
            }
            if !saw_utf8 {
                return Err("expected UTF-8 Pokémon literal in dry plan payload".into());
            }
        }
        "lang_heredoc_binding" => {
            let mut saw_literal = false;
            for nr in &dry.node_results {
                if nr.get("kind").and_then(|k| k.as_str()) != Some("data") {
                    continue;
                }
                let Some(data_v) = nr.get("data") else {
                    continue;
                };
                if json_value_contains_substring(data_v, "hello-matrix") {
                    saw_literal = true;
                    break;
                }
            }
            if !saw_literal {
                return Err("expected data node carrying hello-matrix payload".into());
            }
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!(
                    "expected LangItem query binding, got {:?}",
                    q.entity
                ));
            }
        }
        "lang_derive_map_parallel" => {
            let mut saw_map_object = false;
            for nr in &dry.node_results {
                if nr.get("kind").and_then(|k| k.as_str()) != Some("derive") {
                    continue;
                }
                let Some(v) = nr.get("value") else {
                    continue;
                };
                let Ok(pv) = serde_json::from_value::<PlanValue>(v.clone()) else {
                    continue;
                };
                if let PlanValue::Object { fields } = pv {
                    if fields.contains_key("t") {
                        saw_map_object = true;
                        break;
                    }
                }
            }
            if !saw_map_object {
                return Err("expected derive map object with field t".into());
            }
            let q = first_query(&surfaces)?;
            if q.capability_name.as_ref().map(|c| c.as_str()) != Some("langitem_search") {
                return Err(format!(
                    "expected search capability on hits root, got {:?}",
                    q.capability_name
                ));
            }
        }
        "lang_binding_continuation" => {
            if !surfaces
                .iter()
                .any(|e| expr_contains_get_langitem(e, Some("i1")) || expr_chain_selects_tags(e))
            {
                return Err(format!(
                    "expected LangItem(i1) Get and/or `.tags` navigation surface, got {:?}",
                    surfaces
                ));
            }
            if !surfaces.iter().any(expr_chain_selects_tags)
                && !comp_ir_contains_selector(comp, "tags")
                && !comp_has_relation_named(comp, "tags")
            {
                return Err(format!(
                    "expected `.tags` navigation (surface IR, plan selector walk, or relation node), surfaces={surfaces:?}"
                ));
            }
        }
        "lang_bind_limit1_continuation" => {
            if !comp_has_relation_named(comp, "tags") {
                return Err("expected relation node for `.tags` after limit(1)".to_string());
            }
        }
        "lang_relation_many_from_plural_query" => {
            let rel = comp_relation_named(comp, "tags")
                .ok_or_else(|| "expected `.tags` relation from plural query".to_string())?;
            if rel["source_cardinality"].as_str() != Some("many") {
                return Err(format!(
                    "expected many source_cardinality for plural fanout, got {rel:?}"
                ));
            }
            if rel["source"].as_str() != Some("items") {
                return Err(format!("expected source binding items, got {rel:?}"));
            }
            if rel.pointer("/materialize/kind").and_then(|k| k.as_str())
                != Some("prefer_from_parent_get")
            {
                return Err(format!(
                    "expected prefer_from_parent_get materialize on tags relation, got {rel:?}"
                ));
            }
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!("expected LangItem query, got {:?}", q.entity));
            }
        }
        "lang_relation_prefer_embed_hit" => {
            let rel = comp_relation_named(comp, "tags")
                .ok_or_else(|| "expected `.tags` relation on singleton item".to_string())?;
            if rel.pointer("/materialize/kind").and_then(|k| k.as_str())
                != Some("prefer_from_parent_get")
            {
                return Err(format!(
                    "expected prefer_from_parent_get on embed-hit row, got {rel:?}"
                ));
            }
            if rel["source_cardinality"].as_str() != Some("single") {
                return Err(format!(
                    "expected single source_cardinality for item.tags, got {rel:?}"
                ));
            }
        }
        "lang_relation_prefer_embed_miss" => {
            let rel = comp_relation_named(comp, "tags")
                .ok_or_else(|| "expected `.tags` relation from plural list".to_string())?;
            if rel.pointer("/materialize/kind").and_then(|k| k.as_str())
                != Some("prefer_from_parent_get")
            {
                return Err(format!(
                    "expected prefer_from_parent_get on embed-miss row, got {rel:?}"
                ));
            }
            if rel["source_cardinality"].as_str() != Some("many") {
                return Err(format!(
                    "expected many source_cardinality for plural fanout, got {rel:?}"
                ));
            }
        }
        "lang_bind_plural_relation_opaque_p" | "lang_relation_opaque_r_symbol" => {
            let rel = comp_relation_named(comp, "tags")
                .ok_or_else(|| "expected `.tags` relation from plural binding".to_string())?;
            if rel["source_cardinality"].as_str() != Some("many") {
                return Err(format!(
                    "expected many source_cardinality for opaque plural fanout, got {rel:?}"
                ));
            }
            if rel.pointer("/materialize/kind").and_then(|k| k.as_str())
                != Some("prefer_from_parent_get")
            {
                return Err(format!(
                    "expected prefer_from_parent_get on opaque plural row, got {rel:?}"
                ));
            }
        }
        "lang_flattened_single_liner_coercion" | "lang_flattened_surface_line_compile" => {
            if comp.pointer("/return/step").and_then(|v| v.as_str()) != Some("items") {
                return Err(format!(
                    "flattened single-liner should return first binding `items`, got {:?}",
                    comp.get("return")
                ));
            }
            if comp
                .pointer("/metadata/coerced_default_return")
                .and_then(|v| v.as_str())
                != Some("items")
            {
                return Err(format!(
                    "expected coerced_default_return metadata, got {:?}",
                    comp.get("metadata")
                ));
            }
            let rel = comp_relation_named(comp, "tags")
                .ok_or_else(|| "expected `.tags` relation on flattened program".to_string())?;
            if rel["source"].as_str() != Some("items") {
                return Err(format!(
                    "expected tags relation sourced from items, got {rel:?}"
                ));
            }
        }
        "lang_relation_one_opaque_r" => {
            let rel = comp_relation_named(comp, "summary")
                .ok_or_else(|| "expected `.summary` relation on singleton item".to_string())?;
            if rel["source_cardinality"].as_str() != Some("single") {
                return Err(format!(
                    "expected single source_cardinality for item.summary, got {rel:?}"
                ));
            }
            if rel["cardinality"].as_str() != Some("one") {
                return Err(format!("expected one-cardinality summary rel, got {rel:?}"));
            }
            let mat = rel
                .pointer("/materialize/kind")
                .and_then(|k| k.as_str())
                .unwrap_or("");
            if mat != "from_parent_get" && mat != "prefer_from_parent_get" {
                return Err(format!(
                    "expected embed GET materialize on one-cardinality r# row, got {rel:?}"
                ));
            }
        }
        "lang_homograph_lhs_coercion" => {
            let rel = comp_relation_named(comp, "tags").ok_or_else(|| {
                "expected `.tags` relation from homograph LHS coercion".to_string()
            })?;
            if rel["source"].as_str() != Some("items") {
                return Err(format!(
                    "expected tags relation sourced from items, got {rel:?}"
                ));
            }
            if rel["source_cardinality"].as_str() != Some("many") {
                return Err(format!(
                    "expected many source_cardinality for homograph plural fanout, got {rel:?}"
                ));
            }
        }
        "lang_relation_integer_scoped_bindings" => {
            let rel_plan = comp_relation_named(comp, "tags_by_score").ok_or_else(|| {
                "expected `.tags_by_score` relation with integer scoped bindings".to_string()
            })?;
            if rel_plan["source_cardinality"].as_str() != Some("many") {
                return Err(format!(
                    "expected many source_cardinality for integer binding fanout, got {rel_plan:?}"
                ));
            }
            let proofs = rel_plan
                .get("binding_proofs")
                .and_then(|v| v.as_array())
                .ok_or_else(|| {
                    format!("expected binding_proofs on relation node, got {rel_plan:?}")
                })?;
            if !proofs.iter().any(|p| {
                p.get("cap_param").and_then(|v| v.as_str()) == Some("seq")
                    && p.get("parent_field").and_then(|v| v.as_str()) == Some("score")
            }) {
                return Err(format!("expected seq←score binding proof, got {proofs:?}"));
            }
            let pool: Vec<&Expr> = surfaces.iter().chain(rel.iter()).collect();
            if !pool
                .iter()
                .copied()
                .any(|e| chain_selector_matches(e, "tags_by_score"))
            {
                return Err(format!(
                    "expected `.tags_by_score` chain IR, got surfaces={surfaces:?} rel={rel:?}"
                ));
            }
        }
        "lang_group_by_then_sort_agg_column" => {
            let group = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::GroupBy { .. }))
                .ok_or_else(|| format!("expected GroupBy compute, got {:?}", computes))?;
            let sort = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::Sort { .. }))
                .ok_or_else(|| {
                    format!("expected Sort compute after group_by, got {:?}", computes)
                })?;
            if let ComputeOp::Sort { key, .. } = &sort.op {
                if key.dotted() != "n" {
                    return Err(format!("expected sort on aggregate n, got {:?}", key));
                }
            } else {
                return Err("unreachable".into());
            }
            if let ComputeOp::GroupBy { aggregates, .. } = &group.op {
                if !aggregates.iter().any(|a| a.name.as_str() == "n") {
                    return Err(format!("expected aggregate n, got {:?}", aggregates));
                }
            }
        }
        "lang_dedupe" | "lang_bind_dedupe" => {
            let dedupe = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::DedupeBy { .. }))
                .ok_or_else(|| format!("expected DedupeBy compute, got {:?}", computes))?;
            if let ComputeOp::DedupeBy { keys, .. } = &dedupe.op {
                if keys.is_empty() {
                    return Err("expected non-empty dedupe keys".into());
                }
            }
        }
        "lang_group_by_first" => {
            let group = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::GroupBy { .. }))
                .ok_or_else(|| format!("expected GroupBy compute, got {:?}", computes))?;
            if let ComputeOp::GroupBy { aggregates, .. } = &group.op {
                let first = aggregates
                    .iter()
                    .find(|a| a.function == AggregateFunction::First);
                if first.is_none() {
                    return Err(format!("expected first() aggregate, got {:?}", aggregates));
                }
            }
        }
        "lang_bind_projection_then_relation" => {
            let rel = comp_relation_named(comp, "tags")
                .ok_or_else(|| "expected `.tags` relation after projection anchor".to_string())?;
            if rel["source"].as_str() != Some("trimmed") {
                return Err(format!("expected source trimmed, got {rel:?}"));
            }
        }
        "lang_bind_relation_hop_one_one" => {
            let detail = comp_relation_named(comp, "detail")
                .ok_or_else(|| "expected second one-cardinality `.detail` hop".to_string())?;
            if detail["source_cardinality"].as_str() != Some("single") {
                return Err(format!(
                    "second hop requires single source_cardinality, got {detail:?}"
                ));
            }
            if detail["cardinality"].as_str() != Some("one") {
                return Err(format!(
                    "expected one-cardinality detail rel, got {detail:?}"
                ));
            }
        }
        "lang_federated_relation_target_entry" => {
            let summary = comp_relation_named(comp, "summary")
                .ok_or_else(|| "expected `.summary` relation in federated session".to_string())?;
            if summary.pointer("/target/entry_id").and_then(|v| v.as_str()) != Some("pokeapi") {
                return Err(format!(
                    "relation target must own pokeapi catalog, not primary linear: {summary:?}"
                ));
            }
            if summary.pointer("/target/entity").and_then(|v| v.as_str()) != Some("LangSummary") {
                return Err(format!("expected LangSummary target, got {summary:?}"));
            }
            let ir = summary
                .pointer("/ir/expr")
                .map(|v| v.to_string())
                .unwrap_or_default();
            if ir.contains("\"$\"") {
                return Err("relation IR must not use teaching placeholder $".into());
            }
        }
        "lang_federated_duplicate_entity_e1_query" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!("expected LangItem query on e1, got {:?}", q.entity));
            }
            if q.catalog_entry_id.as_deref() != Some("github") {
                return Err(format!(
                    "e1 must resolve to github catalog, got catalog_entry_id={:?}",
                    q.catalog_entry_id
                ));
            }
            let qe = comp_first_invoke_qualified_entity(comp)
                .ok_or_else(|| "expected qualified_entity on comp invoke step".to_string())?;
            if qe.get("entry_id").and_then(|v| v.as_str()) != Some("github") {
                return Err(format!("comp qualified_entity must be github: {qe:?}"));
            }
            if qe.get("entity").and_then(|v| v.as_str()) != Some("LangItem") {
                return Err(format!("comp qualified_entity entity LangItem: {qe:?}"));
            }
        }
        "lang_federated_duplicate_entity_e2_search" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!(
                    "expected LangItem search on e2, got {:?}",
                    q.entity
                ));
            }
            if q.catalog_entry_id.as_deref() != Some("linear") {
                return Err(format!(
                    "e2 must resolve to linear catalog, got catalog_entry_id={:?}",
                    q.catalog_entry_id
                ));
            }
            let Some(cap) = q.capability_name.as_ref() else {
                return Err("e2 search should pin Search capability".into());
            };
            if cap.as_str() != "langitem_search" {
                return Err(format!("expected langitem_search, got {cap}"));
            }
            let qe = comp_first_invoke_qualified_entity(comp)
                .ok_or_else(|| "expected qualified_entity on comp invoke step".to_string())?;
            if qe.get("entry_id").and_then(|v| v.as_str()) != Some("linear") {
                return Err(format!("comp qualified_entity must be linear: {qe:?}"));
            }
        }
        "lang_federated_duplicate_entity_relation_r" => {
            let rel = comp_relation_named(comp, "children")
                .ok_or_else(|| "expected `.children` relation hop on e2 parent".to_string())?;
            if rel.pointer("/target/entry_id").and_then(|v| v.as_str()) != Some("linear") {
                return Err(format!(
                    "homonymous LangItem relation target must stay on linear catalog: {rel:?}"
                ));
            }
            if rel.pointer("/target/entity").and_then(|v| v.as_str()) != Some("LangItem") {
                return Err(format!("expected LangItem target, got {rel:?}"));
            }
        }
        "lang_federated_duplicate_entity_mutator_m" => {
            let create = surfaces
                .iter()
                .find_map(|e| match e {
                    Expr::Create(c) => Some(c),
                    _ => None,
                })
                .ok_or_else(|| "expected Create surface from e2.m#".to_string())?;
            if create.catalog_entry_id.as_deref() != Some("linear") {
                return Err(format!(
                    "e2 mutator must stamp linear catalog, got {:?}",
                    create.catalog_entry_id
                ));
            }
            if create.capability.as_str() != "langitem_create" {
                return Err(format!(
                    "expected langitem_create, got {}",
                    create.capability
                ));
            }
        }
        "lang_federated_parallel_roots" => {
            if surfaces.len() < 2 {
                return Err(format!(
                    "expected parallel github+linear roots, got {} surfaces",
                    surfaces.len()
                ));
            }
        }
        "lang_federated_group_by_on_e1" => {
            let Some(ComputeTemplate {
                op: ComputeOp::GroupBy { keys, aggregates },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::GroupBy { .. }))
            else {
                return Err(format!("expected GroupBy on e1 query, got {:?}", computes));
            };
            if keys.len() != 1 || keys[0].dotted() != "owner" {
                return Err(format!("expected group key owner, got {:?}", keys));
            }
            if !aggregates.iter().any(|a| a.name.as_str() == "n") {
                return Err(format!("expected aggregate n, got {:?}", aggregates));
            }
            let q = first_query(&surfaces)?;
            if q.catalog_entry_id.as_deref() != Some("github") {
                return Err(format!(
                    "e1 group_by query must be github, got {:?}",
                    q.catalog_entry_id
                ));
            }
        }
        "lang_bind_template_inline_on_e1" => {
            if computes.is_empty() {
                return Err("expected render compute on inline e1 template".into());
            }
            let q = first_query(&surfaces)?;
            if q.catalog_entry_id.as_deref() != Some("github") {
                return Err(format!(
                    "inline template query must be github e1, got {:?}",
                    q.catalog_entry_id
                ));
            }
        }
        "lang_effect_create_literal" => {
            let Some(Expr::Create(c)) = surfaces.iter().find(|e| matches!(e, Expr::Create(_)))
            else {
                return Err(format!("expected Create, got {:?}", surfaces));
            };
            if c.capability.as_str() != "langitem_create" || c.entity != "LangItem" {
                return Err(format!("unexpected create: {:?}", c.capability));
            }
        }
        "lang_effect_update" => {
            let Some(Expr::Invoke(InvokeExpr { capability, .. })) =
                surfaces.iter().find(|e| matches!(e, Expr::Invoke(_)))
            else {
                return Err(format!("expected Invoke IR, got {:?}", surfaces));
            };
            if capability.as_str() != "langitem_update" {
                return Err(format!("expected langitem_update, got {capability}"));
            }
        }
        "lang_effect_action_ping" => {
            let Some(Expr::Invoke(InvokeExpr { capability, .. })) =
                surfaces.iter().find(|e| matches!(e, Expr::Invoke(_)))
            else {
                return Err(format!("expected Invoke IR, got {:?}", surfaces));
            };
            if capability.as_str() != "langitem_ping" {
                return Err(format!("expected langitem_ping, got {capability}"));
            }
        }
        "lang_effect_delete" => {
            let Some(Expr::Delete(d)) = surfaces.iter().find(|e| matches!(e, Expr::Delete(_)))
            else {
                return Err(format!("expected Delete IR, got {:?}", surfaces));
            };
            if d.capability.as_str() != "langitem_delete" {
                return Err(format!("expected langitem_delete, got {:?}", d.capability));
            }
        }
        "lang_for_each_update" => {
            // CGS `update` capabilities lower to `Expr::Invoke`, which [`infer_surface_contract`]
            // classifies as [`PlanNodeKind::Action`] (not `Update`) in the plan DAG.
            let matches_fe_invoke = |nr: &serde_json::Value| {
                (nr.get("kind").and_then(|k| k.as_str()) == Some("for_each")
                    || nr.get("kind").and_then(|k| k.as_str()) == Some("flat_map_effect"))
                    && nr.pointer("/effect_template/kind").and_then(|k| k.as_str())
                        == Some("action")
            };
            let dry_ok = dry.node_results.iter().any(matches_fe_invoke);
            let plan_ok = comp_steps_values(comp).iter().any(|n| matches_fe_invoke(n));
            if !dry_ok && !plan_ok {
                return Err(
                    "expected `for_each` with effect_template.kind action (invoke/update surface)"
                        .into(),
                );
            }
            // Source is a bounded Get + projection (stable Hermit identity); fan-out still exercises
            // `for_each` materialization + invoke templates without relying on generated list rows.
        }
        "lang_domain_symbol_page_size" => {
            let q = first_query(&surfaces)?;
            if q.entity != "LangItem" || q.predicate.is_some() {
                return Err(format!("unexpected query IR: {q:?}"));
            }
            let ps = comp_surface_page_size(comp).ok_or_else(|| {
                "expected plan surface page_size field (IR omits host paging cap)".to_string()
            })?;
            if ps != 10 {
                return Err(format!("expected page_size 10, got {ps}"));
            }
        }
        other => {
            return Err(format!(
                "internal: add IR planning asserts for matrix row {other}"
            ));
        }
    }
    Ok(())
}

fn get_simple_id(g: &GetExpr) -> Option<&str> {
    match &g.reference.key {
        EntityKey::Simple(id) => Some(id.as_str()),
        EntityKey::Compound(_) => None,
    }
}

fn expr_chain_selects_lines(e: &Expr) -> bool {
    chain_selector_matches(e, "lines")
}

fn expr_chain_selects_tags(e: &Expr) -> bool {
    chain_selector_matches(e, "tags")
}

fn chain_selector_matches(e: &Expr, want_sel: &str) -> bool {
    match e {
        Expr::Chain(c) if c.selector == want_sel => true,
        Expr::Chain(c) => {
            chain_selector_matches(&c.source, want_sel)
                || matches!(
                    &c.step,
                    ChainStep::Explicit { expr } if chain_selector_matches(expr, want_sel)
                )
        }
        _ => false,
    }
}

fn expr_contains_get_langitem(e: &Expr, want_id: Option<&str>) -> bool {
    match e {
        Expr::Get(g) if g.reference.entity_type == "LangItem" => {
            want_id.is_none_or(|id| get_simple_id(g) == Some(id))
        }
        Expr::Chain(c) => expr_contains_get_langitem(&c.source, want_id),
        _ => false,
    }
}

fn expr_mentions_langline(e: &Expr) -> bool {
    match e {
        Expr::Query(q) => q.entity == "LangLine",
        Expr::Chain(c) => {
            expr_mentions_langline(&c.source)
                || matches!(
                    &c.step,
                    ChainStep::Explicit { expr } if expr_mentions_langline(expr)
                )
        }
        _ => false,
    }
}

fn assert_row(row: &MatrixRow, out: &PlasmPlanRunResult) -> Result<(), String> {
    if out.node_results.len() < row.min_node_results {
        return Err(format!(
            "row {}: expected at least {} node_results, got {}",
            row.id,
            row.min_node_results,
            out.node_results.len()
        ));
    }
    let md = out.run_markdown.as_deref().unwrap_or("");
    for sub in row.expect_markdown_substrings {
        if !md.contains(sub) {
            return Err(format!(
                "row {}: run_markdown missing substring {sub:?} (len {}):\n{md}",
                row.id,
                md.len()
            ));
        }
    }
    if row.features.iter().any(|f| f.starts_with("relation_")) {
        let return_rows: usize = out.return_steps.iter().map(|s| s.result.count).sum();
        if return_rows == 0 {
            return Err(format!(
                "row {}: relation feature set requires non-zero return step rows (CEP-5/6)",
                row.id
            ));
        }
        if md.contains("(no results)") {
            return Err(format!(
                "row {}: relation live run must not publish (no results)",
                row.id
            ));
        }
    }
    Ok(())
}

const MATRIX_ROWS: &[MatrixRow] = &[
    MatrixRow {
        id: "lang_query_all",
        program: "LangItem",
        surface_line: false,
        federated: false,
        features: &["entity_query", "pagination_fetch_all_default"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_surface_line_limit",
        program: "LangItem.limit(2)",
        surface_line: true,
        federated: false,
        features: &["surface_line_compile", "postfix_limit"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_bind_first_limit",
        program: "items = LangItem\nitems.limit(3)",
        surface_line: false,
        federated: false,
        features: &["bind_first_postfix_limit", "postfix_limit"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_search",
        program: r#"LangItem~"Alpha""#,
        surface_line: false,
        federated: false,
        features: &["entity_search"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "Alpha"],
    },
    MatrixRow {
        id: "lang_get_by_id",
        program: r#"LangItem("i1")"#,
        surface_line: false,
        federated: false,
        features: &["entity_get"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "i1"],
    },
    MatrixRow {
        id: "lang_predicate_brace_owner",
        program: r#"LangItem{owner="alice"}"#,
        surface_line: false,
        federated: false,
        features: &["predicate_brace_equality"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "alice"],
    },
    MatrixRow {
        id: "lang_predicate_brace_score_cmp",
        program: "LangItem{score>1}",
        surface_line: false,
        federated: false,
        features: &["predicate_brace_comparison"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "score"],
    },
    MatrixRow {
        id: "lang_limit_projection",
        program: "LangItem.limit(1)[id,title]",
        surface_line: false,
        federated: false,
        features: &["postfix_limit", "postfix_projection"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "title"],
    },
    MatrixRow {
        id: "lang_sort_limit",
        program: "LangItem.sort(score, desc).limit(2)[id,score]",
        surface_line: false,
        federated: false,
        features: &["postfix_sort"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "score"],
    },
    MatrixRow {
        id: "lang_sort_asc",
        program: "LangItem.sort(score, asc).limit(3)[id,score]",
        surface_line: false,
        federated: false,
        features: &["postfix_sort", "postfix_sort_ascending"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "score"],
    },
    MatrixRow {
        id: "lang_aggregate",
        program: "LangItem.aggregate(n=count)",
        surface_line: false,
        federated: false,
        features: &["postfix_aggregate"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "n"],
    },
    MatrixRow {
        id: "lang_aggregate_sugar_count",
        program: "LangItem.aggregate(count)",
        surface_line: false,
        federated: false,
        features: &["aggregate_sugar_count", "postfix_aggregate"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "count"],
    },
    MatrixRow {
        id: "lang_aggregate_sum",
        program: "LangItem.aggregate(t=sum(score))",
        surface_line: false,
        federated: false,
        features: &["aggregate_sum", "postfix_aggregate"],
        min_node_results: 1,
        // Aggregate label `sum(...)` is not spelled in short markdown; binding `t` is stable.
        expect_markdown_substrings: &["```tsv", "t"],
    },
    MatrixRow {
        id: "lang_group_by",
        program: "LangItem.group_by(owner).aggregate(n=count)",
        surface_line: false,
        federated: false,
        features: &["postfix_group_by", "postfix_group_by_aggregate_chain"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_group_by_aggregate_chain",
        program: "LangItem.group_by(owner, score).aggregate(n=count, title=first(title))",
        surface_line: false,
        federated: false,
        features: &["postfix_group_by_aggregate_chain", "postfix_group_by_multi", "agg_first_last"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_group_by_sugar",
        program: "LangItem.group_by(owner)",
        surface_line: false,
        federated: false,
        features: &["postfix_group_by_sugar", "postfix_group_by"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "count"],
    },
    MatrixRow {
        id: "lang_group_by_multi",
        program: "LangItem.group_by(owner, score, n=count)",
        surface_line: false,
        federated: false,
        features: &["postfix_group_by_multi", "postfix_group_by"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_search_then_group_by",
        program: "rows = LangItem~\"matrix\"\nby_owner = rows.group_by(owner)\nby_owner",
        surface_line: false,
        federated: false,
        features: &["entity_search", "postfix_group_by", "search_then_group_by"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_search_then_group_by_team_key",
        program: "rows = LangItem~\"matrix\"{team_key=\"eng\"}\nby_team = rows.group_by(team_key)\nby_team",
        surface_line: false,
        federated: false,
        features: &["entity_search", "postfix_group_by", "search_then_group_by"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "team_key"],
    },
    MatrixRow {
        id: "lang_row_filter_brace",
        program: "items = LangItem\nfiltered = items.filter{owner=\"alice\"}\nfiltered",
        surface_line: false,
        federated: false,
        features: &["postfix_row_filter", "bindings_assignment"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "alice"],
    },
    MatrixRow {
        id: "lang_row_filter_paren",
        program: "items = LangItem\nfiltered = items.filter(owner=\"alice\")\nfiltered",
        surface_line: false,
        federated: false,
        features: &["postfix_row_filter", "bindings_assignment"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "alice"],
    },
    MatrixRow {
        id: "lang_relation_lines",
        program: r#"LangItem("i1").lines[id,note]"#,
        surface_line: false,
        federated: false,
        features: &["relation_from_parent_get"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "note"],
    },
    MatrixRow {
        id: "lang_query_singleton",
        program: "LangItem.limit(5).singleton()",
        surface_line: false,
        federated: false,
        features: &["postfix_singleton"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_relation_tags_scoped",
        program: r#"LangItem("i1").tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "relation_from_parent_get",
            "relation_prefer_embed_hit",
            "relation_prefer_from_parent_get",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "item_id", "label"],
    },
    MatrixRow {
        id: "lang_bindings_render",
        program: r#"hdr = LangItem("i1")[id,title] <<MD
# {{ rows | length }} row(s): {% for r in rows %}{{ r.id }}{% endfor %}
MD
hdr"#,
        surface_line: false,
        federated: false,
        features: &["bindings_assignment", "bracket_render"],
        min_node_results: 2,
        expect_markdown_substrings: &["row(s)", "```"],
    },
    MatrixRow {
        id: "lang_render_content_into_create",
        program: r#"hdr = LangItem.limit(1)[title] <<PLASM_TITLE_PIPE
{{ rows[0].title }}
PLASM_TITLE_PIPE
LangItem.create(title=hdr.content, score=0, owner="render-pipe-owner")"#,
        surface_line: false,
        federated: false,
        features: &[
            "bracket_render_content_ref",
            "effect_create",
            "bracket_render",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "render-pipe-owner"],
    },
    MatrixRow {
        id: "lang_heredoc_binding",
        program: r#"note = <<PLASM_LANG_MATRIX_EOF
hello-matrix
PLASM_LANG_MATRIX_EOF
one = LangItem.limit(1)[title]
one, note"#,
        surface_line: false,
        federated: false,
        features: &["static_heredoc_binding", "parallel_final_roots"],
        min_node_results: 2,
        expect_markdown_substrings: &["# Results", "hello-matrix", "```tsv"],
    },
    MatrixRow {
        id: "lang_derive_map_parallel",
        program: r#"hits = LangItem~"Alpha"
sumry = hits[id,title]
cards = sumry => { t: _.title }
sumry, cards"#,
        surface_line: false,
        federated: false,
        features: &["derive_map", "parallel_final_roots"],
        min_node_results: 3,
        expect_markdown_substrings: &["# Results", "```tsv", "t"],
    },
    MatrixRow {
        id: "lang_binding_continuation",
        program: r#"root = LangItem("i1")
tags = root.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &["binding_continuation"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_bind_limit1_continuation",
        program: r#"root = LangItem{owner="alice"}
one = root.limit(1)
tags = one.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &["bind_limit1_continuation", "postfix_limit"],
        min_node_results: 3,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_relation_many_from_plural_query",
        program: r#"items = LangItem
tags = items.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "relation_many_from_plural",
            "relation_prefer_from_parent_get",
            "relation_prefer_embed_miss",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
    },
    MatrixRow {
        id: "lang_relation_prefer_embed_hit",
        program: r#"item = LangItem("i1")
tags = item.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "relation_prefer_embed_hit",
            "relation_prefer_from_parent_get",
            "relation_from_parent_get",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "item_id", "label"],
    },
    MatrixRow {
        id: "lang_relation_prefer_embed_miss",
        program: r#"items = LangItem{owner="bob"}
tags = items.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "relation_prefer_embed_miss",
            "relation_prefer_from_parent_get",
            "relation_many_from_plural",
            "relation_query_scoped",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
    },
    MatrixRow {
        id: "lang_bind_plural_relation_opaque_p",
        program: r#"items = LangItem
tags = items.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "binding_opaque_relation_ref",
            "relation_many_from_plural",
            "relation_prefer_from_parent_get",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
    },
    MatrixRow {
        id: "lang_relation_opaque_r_symbol",
        program: "",
        surface_line: false,
        federated: false,
        features: &[
            "relation_opaque_r_symbol",
            "relation_many_from_plural",
            "relation_prefer_from_parent_get",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
    },
    MatrixRow {
        id: "lang_flattened_single_liner_coercion",
        program: "",
        surface_line: false,
        federated: false,
        features: &[
            "flattened_single_liner_coercion",
            "relation_many_from_plural",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "title"],
    },
    MatrixRow {
        id: "lang_flattened_surface_line_compile",
        program: "",
        surface_line: true,
        federated: false,
        features: &[
            "flattened_surface_line_compile",
            "flattened_single_liner_coercion",
            "surface_line_compile",
            "relation_many_from_plural",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "title"],
    },
    MatrixRow {
        id: "lang_relation_one_opaque_r",
        program: "",
        surface_line: false,
        federated: false,
        features: &[
            "relation_one_opaque_r",
            "bind_relation_hop_one_one",
            "relation_from_parent_get",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_homograph_lhs_coercion",
        program: "",
        surface_line: false,
        federated: false,
        features: &[
            "homograph_lhs_coercion",
            "relation_many_from_plural",
            "relation_prefer_from_parent_get",
            "binding_continuation",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
    },
    MatrixRow {
        id: "lang_relation_integer_scoped_bindings",
        program: r#"items = LangItem
tags = items.tags_by_score
tags"#,
        surface_line: false,
        federated: false,
        features: &[
            "relation_many_from_plural",
            "relation_query_scoped_bindings",
            "relation_binding_proof",
            "dry_live_parity",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "label"],
    },
    MatrixRow {
        id: "lang_group_by_then_sort_agg_column",
        program: "LangItem.group_by(owner, n=count).sort(n, desc)",
        surface_line: true,
        federated: false,
        features: &["postfix_group_by", "postfix_group_by_sort", "postfix_sort"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_dedupe",
        program: "LangItem.dedupe(owner).limit(20)",
        surface_line: true,
        federated: false,
        features: &["postfix_dedupe", "postfix_limit"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_bind_dedupe",
        program: "rows = LangItem~\"matrix\"\nrows.dedupe(owner)",
        surface_line: false,
        federated: false,
        features: &["postfix_dedupe", "entity_search", "search_then_group_by"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_group_by_first",
        program: "LangItem.group_by(owner, title=first(title))",
        surface_line: true,
        federated: false,
        features: &["postfix_group_by", "agg_first_last"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_bind_projection_then_relation",
        program: r#"root = LangItem("i1")
trimmed = root[id,title]
tags = trimmed.tags
tags"#,
        surface_line: false,
        federated: false,
        features: &["bind_projection_then_relation", "postfix_projection"],
        min_node_results: 3,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_bind_relation_hop_one_one",
        program: r#"item = LangItem("i1")
summary = item.summary
detail = summary.detail
detail"#,
        surface_line: false,
        federated: false,
        features: &["bind_relation_hop_one_one", "relation_from_parent_get"],
        min_node_results: 3,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_effect_create_literal",
        program: r#"LangItem.create(title="MatrixCreated", score=7, owner="bot")"#,
        surface_line: false,
        federated: false,
        features: &["effect_create"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "MatrixCreated"],
    },
    MatrixRow {
        id: "lang_effect_update",
        program: r#"LangItem("i1").update(title="MatrixPatch", score=42, owner="alice")"#,
        surface_line: false,
        federated: false,
        features: &["effect_update"],
        min_node_results: 1,
        expect_markdown_substrings: &["MatrixPatch", "42"],
    },
    MatrixRow {
        id: "lang_effect_action_ping",
        program: r#"LangItem("i1").ping()"#,
        surface_line: false,
        federated: false,
        features: &["effect_action"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "i1"],
    },
    MatrixRow {
        id: "lang_effect_delete",
        program: r#"LangItem("i2").delete()"#,
        surface_line: false,
        federated: false,
        features: &["effect_delete"],
        min_node_results: 1,
        expect_markdown_substrings: &["(no results)"],
    },
    MatrixRow {
        id: "lang_for_each_update",
        program: "items = LangItem(\"i1\")[id,title,owner]\nsync = items => LangItem(\"i1\").update(score=3, title=_.title, owner=_.owner)\nsync",
        surface_line: false,
        federated: false,
        features: &["for_each_effect"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_federated_relation_target_entry",
        program: r#"item = LangItem("i1")
summary = item.summary
summary"#,
        surface_line: false,
        federated: true,
        features: &["federated_relation_target_entry", "relation_from_parent_get"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_federated_duplicate_entity_e1_query",
        program: r#"e1{owner="alice"}"#,
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "domain_symbol_e1",
            "entity_query",
            "predicate_brace_equality",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_federated_duplicate_entity_e2_search",
        program: "e2~$",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "entity_search",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_federated_duplicate_entity_relation_r",
        program: "",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "federated_duplicate_entity_relation_r",
            "relation_from_parent_get",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_federated_duplicate_entity_mutator_m",
        program: "",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "federated_duplicate_entity_mutator_m",
            "effect_create",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_federated_parallel_roots",
        program: "e1{owner=\"alice\"}, e2~$",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "federated_parallel_roots",
            "entity_query",
            "entity_search",
            "parallel_final_roots",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv"],
    },
    MatrixRow {
        id: "lang_federated_group_by_on_e1",
        program: "by = e1{owner=\"alice\"}.group_by(owner).aggregate(n=count)\nby",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "federated_group_by_on_e1",
            "postfix_group_by_aggregate_chain",
            "postfix_group_by",
        ],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_bind_template_inline_on_e1",
        program: "",
        surface_line: false,
        federated: true,
        features: &[
            "federated_duplicate_entity_symbol",
            "bracket_render_inline_on_e",
            "bracket_render",
            "bindings_assignment",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["row(s)"],
    },
    MatrixRow {
        id: "lang_domain_symbol_page_size",
        program: "e1.page_size(10)",
        surface_line: false,
        federated: false,
        features: &["domain_symbol_e1", "pagination_page_size"],
        min_node_results: 1,
        expect_markdown_substrings: &["```tsv", "owner"],
    },
    MatrixRow {
        id: "lang_utf8_minijinja_dollar_stitch",
        program: r#"type_md = LangItem.limit(1)[title] <<UTF8_ROW_EOF
# Pokémon — {{ rows[0].title }}
UTF8_ROW_EOF
LangItem.create(title=<<UTF8_DOC_EOF
Featured Pokémon
${type_md.content}
UTF8_DOC_EOF
, score=0, owner="utf8-matrix-owner")"#,
        surface_line: false,
        federated: false,
        features: &[
            "utf8_dollar_interpolate",
            "bracket_render",
            "bracket_render_content_ref",
            "effect_create",
            "bindings_assignment",
        ],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "utf8-matrix-owner", "Pokémon"],
    },
];

/// Rows whose `program` is filled at runtime (opaque `r#`, flattened single-liner).
fn matrix_program_for_row(
    row: &MatrixRow,
    es: &plasm_agent::execute_session::ExecuteSession,
) -> String {
    match row.id {
        "lang_relation_opaque_r_symbol" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("matrix session domain exposure");
            let map = exp.symbol_map_arc();
            let r_sym =
                map.ident_sym_relation_for(language_matrix::MATRIX_ENTRY_ID, "LangItem", "tags");
            assert!(
                r_sym.starts_with('r'),
                "expected opaque r# for LangItem.tags, got {r_sym}"
            );
            format!("items = LangItem\ntags = items.{r_sym}\ntags")
        }
        "lang_flattened_single_liner_coercion" | "lang_flattened_surface_line_compile" => {
            // Trailing root `tags` is rewritten to first binding `items`.
            "items = LangItem tags = items.tags tags".to_string()
        }
        "lang_relation_one_opaque_r" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("matrix session domain exposure");
            let map = exp.symbol_map_arc();
            let r_sym =
                map.ident_sym_relation_for(language_matrix::MATRIX_ENTRY_ID, "LangItem", "summary");
            assert!(
                r_sym.starts_with('r'),
                "expected opaque r# for LangItem.summary, got {r_sym}"
            );
            format!("item = LangItem(\"i1\")\nsummary = item.{r_sym}\nsummary")
        }
        "lang_homograph_lhs_coercion" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("matrix session domain exposure");
            let map = exp.symbol_map_arc();
            let p_sym = map.ident_sym_cap_param_for(
                language_matrix::MATRIX_ENTRY_ID,
                "LangItem",
                "langitem_query",
                "tags",
            );
            assert!(
                p_sym.starts_with('p'),
                "expected opaque p# homograph for langitem_query.tags, got {p_sym}"
            );
            format!("items = LangItem\ntags = items.{p_sym}\ntags")
        }
        "lang_federated_duplicate_entity_relation_r" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("federated dup session exposure");
            let map = exp.symbol_map_arc();
            let r_sym = map.ident_sym_relation_for("linear", "LangItem", "children");
            assert!(
                r_sym.starts_with('r'),
                "expected opaque r# for linear LangItem.children, got {r_sym}"
            );
            format!("parent = e2(\"i1\")\nkids = parent.{r_sym}\nkids[id,title]")
        }
        "lang_federated_duplicate_entity_mutator_m" => {
            let exp = es
                .teaching_exposure
                .as_ref()
                .expect("federated dup session exposure");
            let map = exp.symbol_map_arc();
            let m_sym = map.method_sym_for("linear", "LangItem", "create");
            assert!(
                m_sym.starts_with('m'),
                "expected opaque m# for linear LangItem.create, got {m_sym}"
            );
            format!("e2.{m_sym}(title=\"fed-mutator-matrix\", score=0, owner=\"matrix-fed-owner\")")
        }
        "lang_bind_template_inline_on_e1" => r#"report = e1{owner="alice"}[title] <<INLINE_E1
# {{ rows | length }} row(s)
INLINE_E1
report"#
            .to_string(),
        _ => row.program.to_string(),
    }
}

#[tokio::test]
async fn plasm_language_matrix_cgs_templates_validate() {
    let cgs = language_matrix::load_language_matrix_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("capability CML templates");
}

#[test]
fn plasm_language_matrix_live_runs() {
    // Debug builds can overflow the default test thread stack while compiling/running the full matrix.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("matrix live runtime");
            rt.block_on(async {
                let base = hermit_lang_matrix::language_matrix_hermit_base_url()
                    .await
                    .clone();
                plasm_language_matrix_live_runs_impl(base).await;
            });
        })
        .expect("spawn matrix live thread")
        .join()
        .expect("matrix live thread join");
}

async fn matrix_live_run_row(
    row: &'static MatrixRow,
    row_es: &ExecuteSession,
    row_st: &PlasmHostState,
) {
    let program = matrix_program_for_row(row, row_es);
    let bundle = if row.surface_line {
        compile_plasm_expression(
            &PromptPipelineConfig::default(),
            None,
            row_es,
            row.id,
            &program,
        )
    } else {
        compile_plasm_program(
            &PromptPipelineConfig::default(),
            None,
            row_es,
            row.id,
            &program,
        )
    }
    .unwrap_or_else(|e| panic!("row {} compile: {e}", row.id));

    let comp_json = serde_json::to_value(&bundle.artifact().comp)
        .unwrap_or_else(|e| panic!("row {} comp json: {e}", row.id));

    let dry = evaluate_plasm_comp_dry(row_es, &bundle)
        .unwrap_or_else(|e| panic!("row {} evaluate_plasm_comp_dry: {e}", row.id));
    assert_planning_ir(row, &dry, &comp_json)
        .unwrap_or_else(|e| panic!("row {} planning IR: {e}", row.id));
    assert_comp_witness(&dry)
        .unwrap_or_else(|e| panic!("row {} monadic comp witness: {e}", row.id));

    let live = Box::pin(run_plasm_comp(
        row_es,
        row_st,
        row_es.prompt_hash.as_str(),
        "matrix_sess",
        &bundle,
        true,
        None,
        None,
        None,
    ))
    .await
    .unwrap_or_else(|e| panic!("row {} run_plasm_comp: {e}", row.id));

    assert_row(row, &live).unwrap_or_else(|e| panic!("row {} assertion: {e}", row.id));
}

async fn plasm_language_matrix_live_runs_impl(base: String) {
    let cgs = language_matrix::load_language_matrix_cgs();
    plasm_compile::validate_cgs_capability_templates(&cgs).expect("templates");

    let es = language_matrix::matrix_execute_session(cgs.clone());
    let cgs_secondary = language_matrix::load_language_matrix_cgs();
    let es_federated = language_matrix::matrix_federated_relation_target_session(
        cgs.clone(),
        cgs_secondary.clone(),
    );
    let st = language_matrix::matrix_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base.clone()),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs.clone(),
    );
    let st_federated = language_matrix::matrix_federated_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base.clone()),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs.clone(),
        cgs_secondary.clone(),
    );
    let es_federated_dup = language_matrix::matrix_federated_duplicate_entity_session(cgs.clone());
    let st_federated_dup = language_matrix::matrix_federated_duplicate_entity_host_state(
        ExecutionEngine::new(ExecutionConfig {
            base_url: Some(base.clone()),
            ..Default::default()
        })
        .expect("ExecutionEngine"),
        cgs,
    );

    let mut tags_seen: BTreeSet<String> = BTreeSet::new();

    for row in MATRIX_ROWS {
        let (row_es, row_st) = if matches!(
            row.id,
            "lang_federated_duplicate_entity_e1_query"
                | "lang_federated_duplicate_entity_e2_search"
                | "lang_federated_duplicate_entity_relation_r"
                | "lang_federated_duplicate_entity_mutator_m"
                | "lang_federated_parallel_roots"
                | "lang_federated_group_by_on_e1"
                | "lang_bind_template_inline_on_e1"
        ) {
            (&es_federated_dup, &st_federated_dup)
        } else if row.federated {
            (&es_federated, &st_federated)
        } else {
            (&es, &st)
        };
        Box::pin(matrix_live_run_row(row, row_es, row_st)).await;
        for t in row.features {
            tags_seen.insert((*t).to_string());
        }
    }

    let required: BTreeSet<String> = REQUIRED_FEATURE_TAGS
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    tags_seen.insert("host_wait_cancel".to_string());
    tags_seen.insert("monadic_comp_witness".to_string());
    let missing: Vec<_> = required.difference(&tags_seen).cloned().collect();
    assert!(
        missing.is_empty(),
        "missing required feature tag coverage: {missing:?}"
    );
}

/// Host-only `wait` / `cancel` continuations (parse smoke; no Hermit live execute).
#[test]
fn lang_wait_cancel_operation_parse() {
    use plasm_core::expr_parser::parse;
    let cgs = language_matrix::load_language_matrix_cgs();
    let wait = parse("wait(l_AAAAAAAAQACAAAAAAAAAAQ_o1)", cgs.as_ref()).expect("wait parse");
    match wait.expr {
        Expr::Wait(w) => assert_eq!(w.handle.as_str(), "l_AAAAAAAAQACAAAAAAAAAAQ_o1"),
        other => panic!("expected Wait, got {other:?}"),
    }
    let cancel = parse("cancel(l_AAAAAAAAQACAAAAAAAAAAQ_o2)", cgs.as_ref()).expect("cancel parse");
    match cancel.expr {
        Expr::Cancel(c) => assert_eq!(c.handle.as_str(), "l_AAAAAAAAQACAAAAAAAAAAQ_o2"),
        other => panic!("expected Cancel, got {other:?}"),
    }
}
