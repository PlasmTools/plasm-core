//! Planning-IR asserts for matrix rows.

mod effects_program;
mod federated_ra;
mod query_pipe;

use super::ir_helpers::*;
use super::row::MatrixRow;
use plasm_agent::plasm_plan_run::DryPlasmPlanEvaluation;

pub(crate) fn assert_planning_ir(
    row: &MatrixRow,
    dry: &DryPlasmPlanEvaluation,
    comp: &serde_json::Value,
) -> Result<(), String> {
    if row.features.iter().any(|tag| {
        matches!(
            *tag,
            "bound_get_identity" | "bound_query_operand" | "bound_iterate_identity"
        )
    }) {
        let id = if row.features.contains(&"bound_iterate_identity") {
            "cur"
        } else {
            "out"
        };
        let step = comp
            .get("steps")
            .and_then(|s| s.get(id))
            .ok_or("missing bound read step")?;
        if !step["ir"].is_null() || !step["ir_template"].is_object() {
            return Err(format!(
                "bound read must remain ir_template, not concrete dry IR: {step}"
            ));
        }
        let operand = match row.id {
            "lang_bound_get_field" | "lang_bound_query_field" => "source",
            "lang_bound_query_scalar" => "owner",
            _ => "key",
        };
        if comp["bind"]["deps"][id]
            .as_array()
            .is_none_or(|deps| !deps.iter().any(|dep| dep.as_str() == Some(operand)))
        {
            return Err(format!(
                "bound read must retain {operand} dependency: {comp}"
            ));
        }
        if step["ir_template"]["input_bindings"]
            .as_array()
            .is_none_or(|inputs| {
                !inputs
                    .iter()
                    .any(|input| input["from"].as_str() == Some(operand))
            })
        {
            return Err(format!(
                "bound read must bind {operand} into its template: {step}"
            ));
        }
        return Ok(());
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
            _ => return Err("unregistered integer identity expectation".into()),
        };
        if !surface_exprs(dry).iter().any(|expr| matches!(expr, plasm_core::Expr::Get(get) if get_simple_id(get) == Some(expected))) {
            return Err(format!("identity must retain exact integer spelling {expected}"));
        }
        return Ok(());
    }
    if row.features.contains(&"compound_get_identity") {
        let step = &comp["steps"]["out"];
        let bound = row.id == "lang_compound_bound";
        let ir = if bound {
            &step["ir_template"]
        } else {
            &step["ir"]
        };
        let expr: plasm_core::Expr =
            serde_json::from_value(ir["expr"].clone()).map_err(|e| e.to_string())?;
        let plasm_core::Expr::Get(get) = expr else {
            return Err("expected compound Get".into());
        };
        let plasm_core::EntityKey::Compound(slots) = get.reference.key else {
            return Err("compound identity flattened".into());
        };
        if slots.len() != 3 {
            return Err("compound Get must retain all keys".into());
        }
        for (key, literal) in [("owner", "alice"), ("item_id", "i1"), ("name", "main")] {
            let slot = slots.get(key).ok_or("missing compound identity key")?;
            if bound {
                if !matches!(slot, plasm_core::IdentitySlot::Binding(plasm_core::PlasmInputRef::NodeInput { node, path }) if node == "source" && path == &[key.to_string()])
                {
                    return Err(format!(
                        "compound {key} lost its typed source reference: {slot:?}"
                    ));
                }
            } else if slot.as_lit_str() != Some(literal) {
                return Err(format!("compound {key} changed"));
            }
        }
        if bound
            && comp["bind"]["deps"]["out"]
                .as_array()
                .is_none_or(|deps| !deps.iter().any(|dep| dep == "source"))
        {
            return Err("compound Get lost source dependency".into());
        }
        return Ok(());
    }
    if row.id == "lang_group_then_global_aggregate" {
        let steps = comp["steps"].as_object().ok_or("missing comp steps")?;
        let (group_id, group) = steps
            .iter()
            .find(|(_, step)| step["compute"]["op"]["kind"] == "group_by")
            .ok_or("missing group reduction")?;
        let (_, aggregate) = steps
            .iter()
            .find(|(_, step)| step["compute"]["op"]["kind"] == "aggregate")
            .ok_or("missing separate global reduction")?;
        if group["compute"]["op"]["aggregates"][0]["function"] != "count"
            || aggregate["compute"]["source"].as_str() != Some(group_id.as_str())
            || aggregate["compute"]["op"]["aggregates"][0]["function"] != "sum"
            || aggregate["compute"]["op"]["aggregates"][0]["name"] != "total"
        {
            return Err("group count must feed a separate global sum".into());
        }
        return Ok(());
    }
    if row.id == "lang_distinct_projected_values" {
        if !compute_templates(dry)
            .iter()
            .any(|node| matches!(&node.op, plasm_agent::plasm_plan::ComputeOp::DedupeBy { keys } if keys.is_empty()))
        {
            return Err("missing whole-row distinct node".into());
        }
        return Ok(());
    }
    if row.id == "lang_relation_one_chain" {
        if !comp_has_relation_named(comp, "summary") || !comp_has_relation_named(comp, "detail") {
            return Err("one-to-one chain must retain both declared relation hops".into());
        }
        return Ok(());
    }
    if row.id == "lang_relation_empty_fanout" {
        if !comp_has_relation_named(comp, "tags") {
            return Err("missing empty-source relation traversal".into());
        }
        return Ok(());
    }
    let surfaces = surface_exprs(dry);
    let computes = compute_templates(dry);
    let rel = relation_exprs(dry);

    if query_pipe::assert_planning_query_pipe(row, &surfaces, &computes, &rel, dry, comp)?.is_some()
    {
        return Ok(());
    }
    if effects_program::assert_planning_effects_program(row, &surfaces, &computes, &rel, dry, comp)?
        .is_some()
    {
        return Ok(());
    }
    if federated_ra::assert_planning_federated_ra(row, &surfaces, &computes, &rel, dry, comp)?
        .is_some()
    {
        return Ok(());
    }
    Err(format!(
        "internal: add IR planning asserts for matrix row {}",
        row.id
    ))
}
