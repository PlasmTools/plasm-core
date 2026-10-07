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
            // Catalog IL may wrap one clause in conjunction; additional clauses
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
            if !has_scoped_filter(comp) {
                return Err("expected typed scoped predicate filter".into());
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
            require_python_filter(comp)?;
        }
        "lang_with_mul" | "lang_with_div" | "lang_with_concat" | "lang_with_when_len" => {
            if !comp_steps_values(comp).iter().any(|step| {
                step.get("derive")
                    .is_some_and(|derive| derive.get("value").is_some())
            }) {
                return Err("expected recursive value derivation".into());
            }
        }
        "lang_where_literal_boolean_sugar" | "lang_where_boolean_rowset_sugar" => {
            require_python_filter(comp)?;
        }
        "lang_where_in_rowset" | "lang_where_in_rowset_paren" => {
            require_membership(comp, "owner", false)?;
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
            require_membership(comp, "owner", false)?;
        }
        "lang_where_not_in_rowset" => {
            require_membership(comp, "owner", true)?;
        }
        "lang_where_not_in_universe_left" | "lang_where_not_in_universe_right" => {
            require_membership(comp, "title", true)?;
        }
        "lang_quoted_binding_literal" => {
            require_python_filter(comp)?;
            if !json_value_contains_substring(comp, "item") {
                return Err("quoted item literal must remain in the reviewed computation".into());
            }
        }
        "lang_select_alias_where" => {
            if !comp_steps_values(comp).iter().any(|step| {
                step.get("derive")
                    .is_some_and(|derive| derive.get("value").is_some())
            }) {
                return Err("expected recursive value derivation".into());
            }
            require_python_filter(comp)?;
            if !json_value_contains_substring(comp, "handle") {
                return Err("filter must retain the projected handle field".into());
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
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Python { per_row: true, .. }))
            {
                return Err(format!(
                    "expected per-row rendering compute, got {computes:?}"
                ));
            }
        }
        "lang_plain_template_foreach" => {
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Python { per_row: false, .. }))
            {
                return Err("collection rendering must execute one Python reduction".into());
            }
            if !json_value_contains_substring(comp, "items") {
                return Err("plain template must depend on named binding `items`".into());
            }
        }
        "lang_render_relation_shape" => {
            let explicit = computes.iter().any(|c| matches!(&c.op, ComputeOp::Python { per_row: true, source, .. } if c.source == "items" && source.contains("len(row.lines)") && source.contains("relation_count=")));
            if !explicit {
                return Err("expected relation-dependent per-row render".into());
            }
        }
        "lang_render_name_collision" => {
            use plasm_core::plasm_monad::{
                DeriveKind, InputCardinality, PlasmComp, PlasmDataValue, PlasmReturn,
                PlasmStepPayload, ScopedOutput, SyntheticValueKind,
            };
            let typed: PlasmComp = serde_json::from_value(comp.clone())
                .map_err(|error| format!("qualification comp decode: {error}"))?;
            let scopes = typed
                .steps
                .values()
                .filter_map(|step| match step {
                    PlasmStepPayload::MapBody(body) => Some(body.as_ref()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let [scope] = scopes.as_slice() else {
                return Err("qualification requires one bounded row scope".into());
            };
            if scope.parent.source.as_str() != "items"
                || scope.max_parents.get() != 2
                || !matches!(scope.output, ScopedOutput::Record)
                || !scope.captures.is_empty()
            {
                return Err(
                    "qualification scope must own items rows without outer captures".into(),
                );
            }
            scope
                .execution_layers()
                .map_err(|error| format!("qualification scope: {error}"))?;
            let renderers = scope
                .body
                .steps
                .iter()
                .filter_map(|(id, step)| match step {
                    PlasmStepPayload::Map(map)
                        if matches!(map.compute.op, ComputeOp::Python { .. }) =>
                    {
                        Some((id, &map.compute))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            let [(renderer_id, renderer)] = renderers.as_slice() else {
                return Err("qualification requires exactly one scoped renderer".into());
            };
            let ComputeOp::Python {
                per_row: true,
                input_schema: Some(input),
                source,
                ..
            } = &renderer.op
            else {
                return Err("qualification renderer must consume a typed singleton row".into());
            };
            let parent = scope
                .parent
                .contract
                .schema()
                .ok_or("qualification parent schema missing")?;
            if renderer.source != scope.parent.local.as_str()
                || input.fields != parent.fields
                || input.fields.len() != 1
                || input.fields[0].name.as_str() != "title"
                || input.fields[0].value_kind != SyntheticValueKind::String
                || !source.contains("row.title")
            {
                return Err(
                    "qualification renderer must read its parent row title, not outer title".into(),
                );
            }
            let PlasmReturn::Step { step } = &scope.body.return_ else {
                return Err("qualification requires one scoped record return".into());
            };
            let Some(PlasmStepPayload::Derive(record)) = scope.body.steps.get(step.as_str()) else {
                return Err("qualification scoped return must assemble a record".into());
            };
            if record.derive.kind != DeriveKind::Map
                || record.derive.source.as_deref() != Some(scope.parent.local.as_str())
            {
                return Err("qualification record must derive from its scoped parent row".into());
            }
            let [renderer_input] = record.derive.inputs.as_slice() else {
                return Err("qualification record requires exactly one renderer input".into());
            };
            if renderer_input.node.as_str() != renderer_id.as_str()
                || renderer_input.alias != renderer_input.node
                || renderer_input.cardinality != InputCardinality::Singleton
            {
                return Err(
                    "qualification record must consume its scoped singleton renderer".into(),
                );
            }
            let PlasmDataValue::Object { fields } = &record.derive.value else {
                return Err("qualification scoped return must be an object".into());
            };
            if fields.len() != 1
                || !matches!(fields.get("value"),
                Some(PlasmDataValue::NodeSymbol { node, alias, path })
                    if node == *renderer_id && alias == node && path.is_empty())
            {
                return Err("qualification output must use the scoped renderer value".into());
            }
        }
        "lang_cross_binding_render" => {
            let valid = computes.iter().any(|c| match &c.op {
                ComputeOp::Python { per_row: true, .. } => c.source == "a",
                _ => false,
            });
            if !valid {
                return Err(
                    "per-row render must read its explicit a source without collection capture"
                        .into(),
                );
            }
        }
        "lang_render_content_into_create" | "lang_render_content_plural_reject" => {
            let has_create_node = comp_has_invoke_plan_kind(comp, "create");
            if !has_create_node {
                return Err(format!(
                    "expected a comp invoke `create` step (Create may be staged with `ir_template`, not dry `ir.expr`), got {:?}",
                    comp.get("steps")
                ));
            }
            if !computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Python { per_row: true, .. }))
            {
                return Err("expected typed row rendering before create".into());
            }
        }
        _ => return Ok(None),
    }
    Ok(Some(()))
}

fn require_membership(
    comp: &serde_json::Value,
    field: &str,
    _negative: bool,
) -> Result<(), String> {
    require_python_filter(comp)?;
    if !json_value_contains_substring(comp, field) {
        return Err(format!("membership must retain its {field} input"));
    }
    if !comp_steps_values(comp).iter().any(|step| {
        step.pointer("/derive/inputs")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|inputs| {
                inputs
                    .iter()
                    .any(|input| input["cardinality"] == "collection")
            })
    }) {
        return Err("membership must capture a complete collection input".into());
    }
    Ok(())
}
