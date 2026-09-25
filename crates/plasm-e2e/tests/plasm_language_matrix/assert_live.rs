//! Comp witness + live result asserts.

use super::row::MatrixRow;
use plasm_agent::plasm_plan_run::{DryPlasmPlanEvaluation, PlasmPlanRunResult};

pub(crate) fn assert_comp_witness(dry: &DryPlasmPlanEvaluation) -> Result<(), String> {
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

pub(crate) fn assert_row(row: &MatrixRow, out: &PlasmPlanRunResult) -> Result<(), String> {
    if row.id == "lang_group_then_global_aggregate" {
        let step = out.return_steps.first().ok_or("missing global total")?;
        if step.result.entities.len() != 1 {
            return Err("global total must be a singleton".into());
        }
        let total = step.result.entities[0]
            .fields
            .get("total")
            .ok_or("missing total field")?
            .to_value();
        if !matches!(total, plasm_core::Value::Integer(2))
            && !matches!(total, plasm_core::Value::Float(n) if n == 2.0)
        {
            return Err(format!(
                "expected two alpha lanes after group then sum, got {total:?}"
            ));
        }
    }
    if row.id == "lang_distinct_projected_values" {
        let step = out.return_steps.first().ok_or("missing distinct result")?;
        if step.result.entities.len() != 1
            || step.result.entities[0]
                .fields
                .get("shelf")
                .map(|v| v.to_value())
                != Some(plasm_core::Value::String("alpha".into()))
        {
            return Err("distinct must collapse the two alpha shelf values into one row".into());
        }
    }
    if row.id == "lang_relation_empty_fanout"
        && out
            .return_steps
            .iter()
            .any(|step| !step.result.entities.is_empty())
    {
        return Err("empty parent fanout must remain empty".into());
    }
    if row.id == "lang_relation_one_chain" {
        let step = out.return_steps.first().ok_or("missing detail result")?;
        if step.result.entities.len() != 1 || !step.result.entities[0].fields.contains_key("body") {
            return Err(format!(
                "one-to-one chain must return one detail row with body: {:?}",
                step.result.entities
            ));
        }
    }

    if row.id == "lang_apply_query_multirow" {
        let peers = out.return_steps.first().ok_or("missing peers result")?;
        if peers.result.entities.is_empty()
            || peers.result.entities.iter().any(|entity| {
                entity.fields.get("owner").map(|v| v.to_value())
                    != Some(plasm_core::Value::String("alice".into()))
            })
        {
            return Err("per-row Query must retain its source owner selection".into());
        }
    }

    if row.features.contains(&"exact_integer_identity")
        || row.features.contains(&"boolean_identity_literal")
    {
        let expected = match row.id {
            "lang_integer_identity" => "42",
            "lang_boolean_identity_true" => "true",
            "lang_boolean_identity_false" => "false",
            "lang_negative_identity" => "-42",
            "lang_large_identity" => "9007199254740993",
            "lang_min_identity" => "-9223372036854775808",
            _ => return Err("unregistered integer identity".into()),
        };
        let result = out.return_steps.first().ok_or("missing identity result")?;
        let entity = result
            .result
            .entities
            .first()
            .ok_or("missing identity row")?;
        if entity.fields.get("id").map(|v| v.to_value())
            != Some(plasm_core::Value::String(expected.into()))
        {
            return Err(format!(
                "materialized identity lost exact digits: {expected}"
            ));
        }
    }

    if row.features.contains(&"compound_get_identity") {
        for step in &out.return_steps {
            let entity = step
                .result
                .entities
                .first()
                .ok_or("missing compound Get row")?;
            for key in ["owner", "item_id", "name"] {
                if !entity.fields.contains_key(key) {
                    return Err(format!("compound row lost {key}"));
                }
            }
        }
        if row.id == "lang_compound_bound" {
            let [source, result] = out.return_steps.as_slice() else {
                return Err("expected compound source and result".into());
            };
            for key in ["owner", "item_id", "name"] {
                if source.result.entities[0].fields[key] != result.result.entities[0].fields[key] {
                    return Err(format!("compound roundtrip changed {key}"));
                }
            }
        }
    }

    if matches!(
        row.id,
        "lang_bound_get_field"
            | "lang_bound_get_scalar"
            | "lang_bound_query_field"
            | "lang_bound_query_scalar"
    ) {
        let [source, result] = out.return_steps.as_slice() else {
            return Err("bound read must return source and result witnesses".into());
        };
        let source = source.result.entities.first().ok_or("missing source row")?;
        let field = if row.features.contains(&"bound_get_identity") {
            "id"
        } else {
            "owner"
        };
        let expected = source.fields.get(field).ok_or("missing source operand")?;
        if result.result.entities.is_empty()
            || result
                .result
                .entities
                .iter()
                .any(|r| r.fields.get(field) != Some(expected))
        {
            return Err(format!("bound read did not resolve source {field}"));
        }
    }
    if row.id == "lang_iterate_bound_identity" {
        let result = out.return_steps.first().ok_or("missing iteration result")?;
        let current = result
            .result
            .entities
            .first()
            .ok_or("missing observed cursor")?;
        if current.fields.get("phase").map(|v| v.to_value())
            != Some(plasm_core::Value::String("done".into()))
        {
            return Err("bound iteration failed to publish observed terminal state".into());
        }
        if result
            .result
            .operations
            .entries()
            .iter()
            .map(|ack| ack.completed)
            .sum::<usize>()
            != 2
        {
            return Err("bound iteration must complete exactly two writes".into());
        }
    }

    if matches!(
        row.id,
        "lang_take_one_field_bind"
            | "lang_take_one_field_argument"
            | "lang_take_one_field_bound_argument"
    ) {
        let [source, target] = out.return_steps.as_slice() else {
            return Err("bounded extract witness requires source and result roots".into());
        };
        let source_row = source
            .result
            .entities
            .first()
            .ok_or("missing selected row")?;
        let result_row = target
            .result
            .entities
            .first()
            .ok_or("missing extracted result")?;
        let field = if row.id == "lang_take_one_field_bind" {
            "value"
        } else {
            "title"
        };
        let selected_id = source_row.fields.get("id").ok_or("selected row lacks id")?;
        if result_row.fields.get(field) != Some(selected_id) {
            return Err(format!(
                "bounded field extraction changed the selected value: {:?} vs {selected_id:?}",
                result_row.fields
            ));
        }
    }
    if row.id == "lang_bind_filter_continuation" {
        // Continuation is a data contract, independent of the inline preview threshold.
        let tags = out
            .return_steps
            .iter()
            .find(|step| step.entity.as_deref() == Some("LangTag"))
            .ok_or("filtered continuation must return LangTag rows")?;
        if tags.result.entities.is_empty()
            || tags
                .result
                .entities
                .iter()
                .any(|entity| entity.reference.entity_type.as_str() != "LangTag")
        {
            return Err("filtered continuation returned no tags or the wrong entity type".into());
        }
    }
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
    if row.id == "lang_federated_auth_session_provides_mutation" {
        let notes_net: usize = out
            .return_steps
            .iter()
            .filter(|s| s.entity.as_deref() == Some("LangSecuredNote"))
            .map(|s| s.result.stats.network_requests)
            .sum();
        if notes_net < 2 {
            return Err(format!(
                "row {}: inherited summary hydrate must issue search+GET (notes network_requests={notes_net})",
                row.id
            ));
        }
    }
    if matches!(
        row.id,
        "lang_for_each_multirow_update" | "lang_for_each_auth_secured_touch"
    ) {
        // List (+ optional login) plus ≥3 distinct mutator HTTP ops.
        let write_fps: usize = out
            .return_steps
            .iter()
            .flat_map(|s| s.result.request_fingerprints.iter())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let write_net: usize = out
            .return_steps
            .iter()
            .map(|s| s.result.stats.network_requests)
            .sum();
        if write_fps < 3 && write_net < 3 {
            return Err(format!(
                "row {}: expected ≥3 live mutator fanout HTTP ops (distinct fingerprints={write_fps}, network_requests={write_net})",
                row.id
            ));
        }
        let fe_entities: usize = out
            .return_steps
            .iter()
            .filter(|s| {
                s.node_id.as_deref() == Some("done")
                    || s.display.contains("update")
                    || s.display.contains("secured_touch")
            })
            .map(|s| s.result.count)
            .sum();
        if fe_entities < 3 {
            return Err(format!(
                "row {}: expected ≥3 for_each result entities (got {fe_entities}; steps={:?})",
                row.id,
                out.return_steps
                    .iter()
                    .map(|s| (s.node_id.clone(), s.display.clone(), s.result.count))
                    .collect::<Vec<_>>()
            ));
        }
    }
    if row.features.iter().any(|f| f.starts_with("relation_")) {
        let return_rows: usize = out.return_steps.iter().map(|s| s.result.count).sum();
        if return_rows == 0
            && !matches!(
                row.id,
                "lang_federated_relation_target_entry"
                    | "lang_bind_relation_hop_one_one"
                    | "lang_federated_duplicate_entity_relation_r"
                    | "lang_relation_empty_fanout"
            )
        {
            return Err(format!(
                "row {}: relation feature set requires non-zero return step rows (CEP-5/6)",
                row.id
            ));
        }
        if md.contains("(no results)")
            && !matches!(
                row.id,
                "lang_federated_relation_target_entry"
                    | "lang_bind_relation_hop_one_one"
                    | "lang_federated_duplicate_entity_relation_r"
                    | "lang_relation_empty_fanout"
            )
        {
            return Err(format!(
                "row {}: relation live run must not publish (no results)",
                row.id
            ));
        }
    }
    if matches!(
        row.id,
        "lang_where_not_in_universe_left" | "lang_where_not_in_universe_right"
    ) {
        assert_ra13_membership_universe(row.id, out)?;
    }
    if matches!(
        row.id,
        "lang_iterate_until_bound" | "lang_iterate_until_zero_step"
    ) {
        if md.contains("(no results)") {
            return Err(format!(
                "row {}: successful iterate must publish rematerialized seed rows, not (no results)",
                row.id
            ));
        }
        let done = out
            .return_steps
            .iter()
            .find(|s| s.node_id.as_deref() == Some("done"))
            .ok_or_else(|| format!("row {}: missing done return step", row.id))?;
        if done.result.entities.is_empty() || done.result.count != done.result.entities.len() {
            return Err(format!(
                "row {}: iterate done advertised count={} entities={} (HTTP-2)",
                row.id,
                done.result.count,
                done.result.entities.len()
            ));
        }
        if row.id == "lang_iterate_until_bound" && done.result.operations.is_empty() {
            return Err(format!(
                "row {}: iterate steps ran but operations ledger is empty",
                row.id
            ));
        }
        if row.id == "lang_iterate_until_zero_step" && !done.result.operations.is_empty() {
            return Err(format!(
                "row {}: zero-step iterate must not mint step acks (got {:?})",
                row.id,
                done.result.operations.entries()
            ));
        }
    }
    Ok(())
}

fn assert_ra13_membership_universe(row_id: &str, out: &PlasmPlanRunResult) -> Result<(), String> {
    let titles: Vec<String> = out
        .return_steps
        .iter()
        .flat_map(|s| s.result.entities.iter())
        .filter_map(|e| {
            e.fields
                .get("title")
                .and_then(|v| v.to_value().as_string_or_phrase().map(str::to_string))
        })
        .collect();
    let entities: Vec<String> = out
        .return_steps
        .iter()
        .filter_map(|s| s.entity.clone())
        .collect();
    let has = |t: &str| titles.iter().any(|x| x == t);
    match row_id {
        "lang_where_not_in_universe_left" => {
            if !entities.iter().any(|e| e == "LangLane") {
                return Err(format!(
                    "RA-13 universe: pipe-left must stay LangLane, got {entities:?}"
                ));
            }
            if entities.iter().any(|e| e == "LangLaneStock") {
                return Err(format!(
                    "RA-13 universe: left polarity must not return LangLaneStock, got {entities:?}"
                ));
            }
            if !has("alpha-two") {
                return Err(format!(
                    "RA-13 universe: A minus B must keep A-only title alpha-two, got {titles:?}"
                ));
            }
            if has("alpha-one") || has("stock-one") || has("stock-two") {
                return Err(format!(
                    "RA-13 universe: A minus B must drop shared/B titles, got {titles:?}"
                ));
            }
        }
        "lang_where_not_in_universe_right" => {
            if !entities.iter().any(|e| e == "LangLaneStock") {
                return Err(format!(
                    "RA-13 universe: pipe-left must stay LangLaneStock, got {entities:?}"
                ));
            }
            if entities.iter().any(|e| e == "LangLane") {
                return Err(format!(
                    "RA-13 universe: right polarity must not return LangLane, got {entities:?}"
                ));
            }
            if !has("stock-one") || !has("stock-two") {
                return Err(format!(
                    "RA-13 universe: B minus A must keep B-only titles, got {titles:?}"
                ));
            }
            if has("alpha-one") || has("alpha-two") {
                return Err(format!(
                    "RA-13 universe: B minus A must drop shared/A titles, got {titles:?}"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}
