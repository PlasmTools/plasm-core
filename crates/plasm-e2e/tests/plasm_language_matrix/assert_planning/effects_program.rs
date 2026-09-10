//! Planning-IR assert arms (auto-split from monolith).

use super::super::ir_helpers::*;
use super::super::row::MatrixRow;
use plasm_agent::plasm_plan::{AggregateFunction, ComputeOp, ComputeTemplate, PlanValue};
use plasm_agent::plasm_plan_run::DryPlasmPlanEvaluation;
use plasm_core::{Expr, InvokeExpr};

pub(crate) fn assert_planning_effects_program(
    row: &MatrixRow,
    surfaces: &[Expr],
    computes: &[ComputeTemplate],
    rel: &[Expr],
    dry: &DryPlasmPlanEvaluation,
    comp: &serde_json::Value,
) -> Result<Option<()>, String> {
    match row.id {
        "lang_utf8_minijinja_content_stitch" => {
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
            let q = first_query(surfaces)?;
            if q.entity != "LangItem" {
                return Err(format!(
                    "expected LangItem query binding, got {:?}",
                    q.entity
                ));
            }
        }
        "lang_heredoc_into_create" => {
            if !comp_has_invoke_plan_kind(comp, "create")
                && !surfaces
                    .iter()
                    .any(|e| matches!(e, Expr::Create(_) | Expr::Invoke(_)))
            {
                return Err("expected LangItem.create from heredoc string binding".into());
            }
            if !json_value_contains_substring(comp, "hello-heredoc-string")
                && !dry
                    .node_results
                    .iter()
                    .any(|nr| json_value_contains_substring(nr, "hello-heredoc-string"))
            {
                return Err("expected heredoc string body wired into create title".into());
            }
            // Option A: bare `title=body` — empty path on the body node_input, not `.content`.
            let comp_s = serde_json::to_string(comp).unwrap_or_default();
            if comp_s.contains(r#""node":"body""#)
                && comp_s.contains(r#""path":["content"]"#)
                && comp_s.matches(r#""node":"body""#).count()
                    <= comp_s.matches(r#""path":["content"]"#).count()
            {
                // Only fail when body is paired with a content path in the same IR blob heuristically:
                // prefer explicit hole shape check below.
            }
            if comp_s.contains(r#""node":"body","path":["content"]"#)
                || comp_s.contains(r#""node": "body", "path": ["content"]"#)
            {
                return Err("option A forbids body.content path on literal heredoc bind".into());
            }
        }
        "lang_heredoc_body_with_equals" => {
            let mut saw_body = false;
            for nr in &dry.node_results {
                if json_value_contains_substring(nr, "key = value") {
                    saw_body = true;
                    break;
                }
            }
            if !saw_body {
                return Err("expected heredoc body with interior equals in dry payload".into());
            }
        }
        "lang_inline_heredoc_method_arg" => {
            if !comp_has_invoke_plan_kind(comp, "create") {
                return Err("expected create invoke with inline heredoc arg".into());
            }
            if !json_value_contains_substring(comp, "line one") {
                return Err("expected inline heredoc body preserved in comp IR (PLP-2)".into());
            }
        }
        "lang_inline_heredoc_method_arg_same_line" => {
            if !comp_has_invoke_plan_kind(comp, "create") {
                return Err("expected create invoke with same-line heredoc close (PLP-2)".into());
            }
            if !json_value_contains_substring(comp, "same-line body") {
                return Err("expected same-line heredoc body preserved in comp IR".into());
            }
        }
        "lang_inline_heredoc_method_arg_github_shape" => {
            if !comp_has_invoke_plan_kind(comp, "create") {
                return Err(
                    "expected create invoke with github-shaped heredoc close (PLP-2)".into(),
                );
            }
            if !json_value_contains_substring(comp, "## Problem") {
                return Err("expected markdown heredoc body preserved in comp IR".into());
            }
            if !json_value_contains_substring(comp, "documentation") {
                return Err("expected array arg after heredoc close in comp IR".into());
            }
        }
        "lang_bind_method_invoke_field_ref" => {
            let Some(Expr::Invoke(InvokeExpr { capability, .. })) =
                surfaces.iter().find(|e| matches!(e, Expr::Invoke(_)))
            else {
                return Err("expected update invoke from bound method continuation".into());
            };
            if capability.as_str() != "langitem_update" {
                return Err(format!("expected langitem_update, got {capability}"));
            }
            if !json_value_contains_substring(comp, "peer") {
                return Err(
                    "expected bound method invoke field-ref lowered from peer binding (PLP-1)"
                        .into(),
                );
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
            let q = first_query(surfaces)?;
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
        "lang_pipe_select_row_fields" => {
            if comp_has_relation_named(comp, "title") {
                return Err("select title must not lower `title` as a relation".into());
            }
            let Some(ComputeTemplate {
                op: ComputeOp::Project { fields },
                ..
            }) = computes
                .iter()
                .find(|c| matches!(c.op, ComputeOp::Project { .. }))
            else {
                return Err(format!(
                    "expected Project compute from `| select title`, got {computes:?}"
                ));
            };
            if !fields.keys().any(|k| k.as_str() == "title") {
                return Err(format!(
                    "expected Project fields to include title, got {fields:?}"
                ));
            }
        }
        "lang_bind_singleton_field_scalar" => {
            if computes
                .iter()
                .any(|c| matches!(c.op, ComputeOp::Project { .. }))
            {
                return Err(
                    "StaticSingleton `.title` must be scalar Derive, not Project / `| select`"
                        .into(),
                );
            }
            let has_title_derive = comp
                .get("steps")
                .and_then(|s| s.as_object())
                .and_then(|steps| steps.get("title"))
                .is_some_and(|step| step.get("kind").and_then(|k| k.as_str()) == Some("derive"));
            if !has_title_derive {
                return Err(format!(
                    "expected derive step `title` for singleton field-dot scalar, got {comp:?}"
                ));
            }
        }
        "lang_bind_limit1_continuation" => {
            if !comp_has_relation_named(comp, "tags") {
                return Err("expected relation node for `=> _.tags` after `| take 1`".to_string());
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
            let q = first_query(surfaces)?;
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
        "lang_program_return_binding_only_last" => {
            if comp.pointer("/return/step").and_then(|v| v.as_str()) != Some("limited") {
                return Err(format!(
                    "binding-only omission should return last binding `limited`, got {:?}",
                    comp.get("return")
                ));
            }
            if comp
                .pointer("/metadata/coerced_default_return")
                .and_then(|v| v.as_str())
                != Some("limited")
            {
                return Err(format!(
                    "expected coerced_default_return `limited`, got {:?}",
                    comp.get("metadata")
                ));
            }
        }
        "lang_program_return_pipeline_filter_sort" => {
            if comp.pointer("/return/step").and_then(|v| v.as_str()) != Some("sorted") {
                return Err(format!(
                    "pipeline binding-only omission should return `sorted`, got {:?}",
                    comp.get("return")
                ));
            }
            if comp
                .pointer("/metadata/coerced_default_return")
                .and_then(|v| v.as_str())
                != Some("sorted")
            {
                return Err(format!(
                    "expected coerced_default_return `sorted`, got {:?}",
                    comp.get("metadata")
                ));
            }
        }
        "lang_program_return_consecutive_writes" => {
            if comp.pointer("/return/kind").and_then(|v| v.as_str()) != Some("parallel") {
                return Err(format!(
                    "expected parallel multi-root return, got {:?}",
                    comp.get("return")
                ));
            }
            let deps = comp
                .pointer("/bind/deps/newfile")
                .and_then(|v| v.as_array())
                .ok_or_else(|| "expected bind.deps[newfile] program-order edge".to_string())?;
            if !deps.iter().any(|d| d.as_str() == Some("newbranch")) {
                return Err(format!(
                    "expected newbranch in bind.deps[newfile] (program-order writes), got {deps:?}"
                ));
            }
            let meta = comp
                .pointer("/metadata/program_order_write_deps")
                .and_then(|v| v.as_array())
                .ok_or_else(|| {
                    "expected comp.metadata.program_order_write_deps witness".to_string()
                })?;
            if meta != &[serde_json::json!(["newbranch", "newfile"])] {
                return Err(format!(
                    "unexpected program_order_write_deps metadata: {meta:?}"
                ));
            }
            let layers = dry
                .graph_summary
                .get("execution_layers")
                .ok_or_else(|| "expected graph_summary.execution_layers".to_string())?;
            if layers != &serde_json::json!([["newbranch"], ["newfile"]]) {
                return Err(format!("expected sequential write layers, got {layers:?}"));
            }
        }
        "lang_bind_relation_hop_one_one" => {
            let pool: Vec<&Expr> = surfaces.iter().chain(rel.iter()).collect();
            if !pool
                .iter()
                .copied()
                .any(|e| matches!(e, Expr::Chain(c) if c.selector.as_str() == "summary"))
            {
                return Err(format!(
                    "expected `.summary` chain IR, got surfaces={surfaces:?} rel={rel:?}"
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
        _ => return Ok(None),
    }
    Ok(Some(()))
}
