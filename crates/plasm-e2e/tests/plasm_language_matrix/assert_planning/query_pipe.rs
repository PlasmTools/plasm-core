//! Planning-IR assert arms (auto-split from monolith).

use super::super::ir_helpers::*;
use super::super::row::MatrixRow;
use plasm_agent::plasm_plan::{AggregateFunction, ComputeOp, ComputeTemplate};
use plasm_agent::plasm_plan_run::DryPlasmPlanEvaluation;
use plasm_core::{CompOp, Expr, Predicate};

pub(crate) fn assert_planning_query_pipe(
    row: &MatrixRow,
    surfaces: &[Expr],
    computes: &[ComputeTemplate],
    rel: &[Expr],
    _dry: &DryPlasmPlanEvaluation,
    comp: &serde_json::Value,
) -> Result<Option<()>, String> {
    match row.id {
        "lang_query_all" => {
            let q = first_query(surfaces)?;
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
            let q = first_query(surfaces)?;
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
            let q = first_query(surfaces)?;
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
        "lang_search_miss" => {
            let q = first_query(surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!("expected LangItem, got {:?}", q.entity));
            }
            let Some(cap) = q.capability_name.as_ref() else {
                return Err("miss search must pin langitem_search, not Query::all".into());
            };
            if cap.as_str() != "langitem_search" {
                return Err(format!("expected langitem_search, got {cap}"));
            }
            let Some(pred) = q.predicate.as_ref() else {
                return Err("expected search predicate on miss literal".into());
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
            if tcv_string(value).as_deref() != Some("no-such-item") {
                return Err(format!(
                    "expected no-such-item search text, got {:?}",
                    tcv_string(value)
                ));
            }
        }
        "lang_search_brace_q" => {
            let q = first_query(surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!("expected LangItem, got {:?}", q.entity));
            }
            let Some(cap) = q.capability_name.as_ref() else {
                return Err("brace {{q=}} must resolve to Search, not primary Query".into());
            };
            if cap.as_str() != "langitem_search" {
                return Err(format!("RA-2: {{q=}} must be langitem_search, got {cap}"));
            }
            let Some(pred) = q.predicate.as_ref() else {
                return Err("expected q predicate".into());
            };
            let Predicate::Comparison {
                field,
                op: CompOp::Eq,
                value,
            } = pred
            else {
                return Err(format!("expected equality predicate, got {pred:?}"));
            };
            if field != "q" || tcv_string(value).as_deref() != Some("Alpha") {
                return Err(format!("expected q=Alpha, got {field}={value:?}"));
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
            let q = first_query(surfaces)?;
            // `capability_name` may be inferred later in the pipeline; brace IR stability is the predicate.
            let Some(pred) = q.predicate.as_ref() else {
                return Err("expected owner predicate".into());
            };
            // Both frontends may wrap one clause in conjunction; additional clauses
            // remain a mismatch rather than being ignored by this assertion.
            let pred = match pred {
                Predicate::And { args } if args.len() == 1 => &args[0],
                other => other,
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
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Filter { .. }))
            {
                return Err(format!("expected row Filter compute, got {computes:?}"));
            }
        }
        "lang_limit_projection" => {
            let q = first_query(surfaces)?;
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
        "lang_with_mul" | "lang_with_div" | "lang_with_concat" | "lang_with_when_len" => {
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::With { .. }))
            {
                return Err(format!("expected With compute, got {:?}", computes));
            }
        }
        "lang_where_literal_boolean_sugar" | "lang_where_boolean_rowset_sugar" => {
            if !computes.iter().any(|c| matches!(&c.op, ComputeOp::Filter { predicates } if predicates.conjunction().is_none())) {
                return Err("boolean repair must preserve non-conjunctive typed filter structure".into());
            }
        }
        "lang_where_in_rowset" | "lang_where_in_rowset_paren" => {
            let Some(ComputeOp::Filter { predicates }) =
                computes.iter().map(|c| &c.op).find(|op| {
                    matches!(
                        op,
                        ComputeOp::Filter { predicates } if predicates.iter().any(|p| {
                            format!("{p:?}").contains("In") && !format!("{p:?}").contains("NotIn")
                        })
                    )
                })
            else {
                return Err(format!(
                    "RA-13: expected `| where owner in …` Filter, got {computes:?}"
                ));
            };
            let dbg = format!("{predicates:?}");
            if !dbg.contains("owner") {
                return Err(format!(
                    "RA-13: membership Filter must bind `owner`, got {dbg}"
                ));
            }
        }
        "lang_union_empty_right" => {
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Union { .. }))
            {
                return Err(format!(
                    "RA-14 empty-right: expected `| union` ComputeOp::Union, got {computes:?}"
                ));
            }
        }
        "lang_required_selection_default" => {
            let q = first_query(surfaces)?;
            if q.entity != "LangLaneStock" {
                return Err(format!("expected LangLaneStock query, got {:?}", q.entity));
            }
            let pred = format!("{:?}", q.predicate);
            if !pred.contains("shelf") || !pred.contains("mine") {
                return Err(format!(
                    "RA-15: omitted shelf must receive authored default mine, got {pred}"
                ));
            }
        }
        "lang_required_selection_multi" | "lang_required_selection_empty" => {
            let q = first_query(surfaces)?;
            if q.entity != "LangLane" {
                return Err(format!("expected LangLane query, got {:?}", q.entity));
            }
            let pred = format!("{:?}", q.predicate);
            if !pred.contains("shelf") {
                return Err(format!("RA-15: LangLane query must keep shelf, got {pred}"));
            }
        }
        "lang_union_rowset"
        | "lang_union_rowset_alias"
        | "lang_union_rowset_alias_distinct"
        | "lang_union_rowset_alias_existing"
        | "lang_union_rowset_paren" => {
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Union { .. }))
            {
                return Err(format!(
                    "RA-14: expected `| union` ComputeOp::Union, got {computes:?}"
                ));
            }
            if !computes.iter().any(|c| {
                matches!(
                    &c.op,
                    ComputeOp::Filter { predicates } if predicates.iter().any(|p| {
                        format!("{p:?}").contains("In") && !format!("{p:?}").contains("NotIn")
                    })
                )
            }) {
                return Err(format!(
                    "RA-14: union result must be a lawful RA-13 RHS, got {computes:?}"
                ));
            }
        }
        "lang_where_not_in_rowset" => {
            let Some(ComputeOp::Filter { predicates }) =
                computes.iter().map(|c| &c.op).find(|op| {
                    matches!(
                        op,
                        ComputeOp::Filter { predicates } if predicates
                            .iter()
                            .any(|p| format!("{p:?}").contains("NotIn"))
                    )
                })
            else {
                return Err(format!(
                    "RA-13: expected `| where owner not in …` Filter, got {computes:?}"
                ));
            };
            let dbg = format!("{predicates:?}");
            if !dbg.contains("owner") {
                return Err(format!(
                    "RA-13: anti-join Filter must bind `owner`, got {dbg}"
                ));
            }
        }
        "lang_where_not_in_universe_left" | "lang_where_not_in_universe_right" => {
            let Some(ComputeOp::Filter { predicates }) =
                computes.iter().map(|c| &c.op).find(|op| {
                    matches!(
                        op,
                        ComputeOp::Filter { predicates } if predicates
                            .iter()
                            .any(|p| format!("{p:?}").contains("NotIn"))
                    )
                })
            else {
                return Err(format!(
                    "RA-13 universe: expected `| where title not in …` Filter, got {computes:?}"
                ));
            };
            let dbg = format!("{predicates:?}");
            if !dbg.contains("title") {
                return Err(format!(
                    "RA-13 universe: anti-join Filter must bind `title`, got {dbg}"
                ));
            }
        }
        "lang_quoted_binding_literal" => {
            let Some(ComputeOp::Filter { predicates }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|op| matches!(op, ComputeOp::Filter { .. }))
            else {
                return Err(format!(
                    "PLP-11: expected `| where title = \"item\"` Filter, got {computes:?}"
                ));
            };
            let dbg = format!("{predicates:?}");
            if !dbg.contains("title") || !dbg.contains("item") {
                return Err(format!(
                    "PLP-11: filter must keep the quoted literal `item`, got {dbg}"
                ));
            }
            if dbg.contains("BindingSymbol") {
                return Err(format!(
                    "PLP-11: quoted `item` must stay a literal, not a binding, got {dbg}"
                ));
            }
        }
        "lang_select_alias_where" => {
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::With { .. }))
            {
                return Err(format!("expected With compute, got {:?}", computes));
            }
            let Some(ComputeOp::Filter { predicates }) = computes
                .iter()
                .map(|c| &c.op)
                .find(|op| matches!(op, ComputeOp::Filter { .. }))
            else {
                return Err(format!(
                    "expected Filter on select alias, got {:?}",
                    computes
                ));
            };
            let predicate_debug = format!("{predicates:?}");
            if !predicate_debug.contains("handle") {
                return Err(format!(
                    "RA-2: filter must bind select alias `handle`, got {predicate_debug}"
                ));
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
            let q = first_query(surfaces)?;
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
            let q = first_query(surfaces)?;
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
            let q = first_query(surfaces)?;
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
        "lang_render_derived_shape"
        | "lang_render_value_error_at_execution"
        | "lang_render_projected_shape"
        | "lang_bindings_render"
        | "lang_render_split_part"
        | "lang_per_row_render_zero"
        | "lang_per_row_render_many" => {
            if !computes.iter().any(|c| matches!(c.op, ComputeOp::Render { .. } | ComputeOp::Python { per_row: true, .. })) {
                return Err(format!("expected per-row rendering compute, got {computes:?}"));
            }
        }
        "lang_plain_template_foreach" => {
            if computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Render { .. }))
            {
                return Err(
                    "plain `{% for item in items %}` must evaluate once as a Data template, not per-row Render"
                        .into(),
                );
            }
            if !json_value_contains_substring(comp, "items") {
                return Err("plain template must depend on named binding `items`".into());
            }
        }
        "lang_render_relation_shape" => {
            let native = computes.iter().any(|c| matches!(c.op, ComputeOp::Render { .. }));
            let explicit = computes.iter().any(|c| matches!(&c.op, ComputeOp::Python { per_row: true, source, .. } if c.source == "items" && source.contains("len(row.lines)") && source.contains("relation_count=")));
            if !native && !explicit { return Err("expected relation-dependent per-row render".into()); }
        }
        "lang_render_name_collision" => {
            if !computes.iter().any(|c| matches!(&c.op, ComputeOp::Python { per_row: true, source, .. } if c.source == "items" && source.contains("row.title"))) {
                return Err("Python qualification must render items.row.title without capturing the outer title binding".into());
            }
        }
        "lang_cross_binding_render" => {
            let valid = computes.iter().any(|c| match &c.op {
                ComputeOp::Render { render_bindings, .. } => render_bindings.is_empty(),
                ComputeOp::Python { per_row: true, .. } => c.source == "a",
                _ => false,
            });
            if !valid { return Err("per-row render must read its explicit a source without collection capture".into()); }
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
                .any(|c| matches!(c.op, ComputeOp::Render { .. } | ComputeOp::Python { per_row: true, .. }))
            {
                return Err("expected typed row rendering before create".into());
            }
        }
        _ => return Ok(None),
    }
    Ok(Some(()))
}
