//! Staged plan input row materialization and cardinality gates.

use super::super::*;
use super::hole_paths::NodeInputHoleIndex;
use std::collections::BTreeMap;

pub(crate) fn materialized_input_row_from_mat(
    node: PlanNodeId,
    mat: &MaterializedNode,
    proof: crate::plasm_plan::InputCardinalityProof,
) -> Result<MaterializedInputRow, String> {
    let inline = mat.row_source.inline_rows().ok_or_else(|| {
        format!(
            "plan input node {:?} has no inline rows for staging",
            node.as_str()
        )
    })?;
    if inline.is_empty() {
        return Err(format!(
            "Plan input {:?} expected at least one row but was empty",
            node.as_str()
        ));
    }
    let mut rows = Vec::with_capacity(inline.len());
    let mut row_identities = Vec::with_capacity(inline.len());
    for (idx, row) in inline.iter().enumerate() {
        let ident = mat.row_identities.get(idx).cloned().flatten();
        rows.push(
            crate::plasm_plan_run::row_json::augment_row_json_with_identity(row, ident.as_ref()),
        );
        row_identities.push(ident);
    }
    Ok(MaterializedInputRow {
        node,
        proof,
        row: rows[0].clone(),
        rows,
        row_identity: row_identities.first().cloned().flatten(),
        row_identities,
    })
}

pub(crate) fn materialized_singleton_inputs(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    inputs: &[ValidatedPlanDataInput],
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let mut out = BTreeMap::new();
    for input in inputs {
        let node = input.node.clone();
        let alias = input.alias.clone();
        let mat = materialized.get(&node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                node.as_str(),
                alias.as_str()
            )
        })?;
        if mat.inline_row_count() != 1 {
            return Err(singleton_input_row_count_error(
                node.as_str(),
                alias.as_str(),
                mat.inline_row_count(),
                format!("{:?} broadcast", input.proof).as_str(),
            ));
        }
        out.insert(
            input.alias.clone(),
            materialized_input_row_from_mat(input.node.clone(), mat, input.proof)?,
        );
    }
    Ok(out)
}

pub(crate) fn materialized_result_use_inputs(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    uses_result: &[PlanResultUse],
    template: Option<&ValidatedPlanExprTemplate>,
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let holes = template.map(|t| NodeInputHoleIndex::from_template_expr(&t.expr));
    let mut out = BTreeMap::new();
    for use_result in uses_result {
        let node = PlanNodeId::new(use_result.node.clone())?;
        let alias = InputAlias::new(use_result.r#as.clone())?;
        let mat = materialized.get(&node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                node.as_str(),
                alias.as_str()
            )
        })?;
        let row_count = mat.inline_row_count();
        let needs_singleton = holes
            .as_ref()
            .map(|h| h.needs_singleton_row(&alias))
            .unwrap_or(true);
        if row_count == 0 {
            return Err(singleton_input_row_count_error(
                node.as_str(),
                alias.as_str(),
                row_count,
                "staged expression rendering",
            ));
        }
        if needs_singleton && row_count != 1 {
            return Err(singleton_input_row_count_error(
                node.as_str(),
                alias.as_str(),
                row_count,
                "staged expression rendering",
            ));
        }
        out.insert(
            alias,
            materialized_input_row_from_mat(
                node,
                mat,
                crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton,
            )?,
        );
    }
    Ok(out)
}

pub(crate) fn singleton_input_row_count_error(
    node: &str,
    alias: &str,
    row_count: usize,
    context: &str,
) -> String {
    if row_count == 0 {
        format!(
            "Plan input {node:?} for alias {alias:?} expected exactly one row for {context}, but the source produced zero rows. This is a data-empty result, not a Plasm syntax error: run or inspect {node:?}, loosen filters if it should match, branch around empty results, or use `.singleton()` only when exactly one row is guaranteed."
        )
    } else {
        format!(
            "Plan input {node:?} for alias {alias:?} expected exactly one row for {context}, but the source produced {row_count} rows. Add filters/projection to make the source unique, aggregate intentionally, or use `.singleton()` only when exactly one row is guaranteed."
        )
    }
}

pub(crate) fn materialized_result_use_inputs_with_source_row(
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    uses_result: &[PlanResultUse],
    source_node: &PlanNodeId,
    source_row: &serde_json::Value,
    source_row_identity: Option<plasm_core::RowIdentity>,
) -> Result<BTreeMap<InputAlias, MaterializedInputRow>, String> {
    let mut out = BTreeMap::new();
    for use_result in uses_result {
        let node = PlanNodeId::new(use_result.node.clone())?;
        let alias = InputAlias::new(use_result.r#as.clone())?;
        let mat = materialized.get(&node).ok_or_else(|| {
            format!(
                "input node {:?} for alias {:?} has not been materialized",
                node.as_str(),
                alias.as_str()
            )
        })?;
        let input_row = if node == *source_node {
            let row = crate::plasm_plan_run::row_json::augment_row_json_with_identity(
                source_row,
                source_row_identity.as_ref(),
            );
            MaterializedInputRow {
                node,
                proof: crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton,
                rows: vec![row.clone()],
                row,
                row_identity: source_row_identity.clone(),
                row_identities: vec![source_row_identity.clone()],
            }
        } else {
            if mat.inline_row_count() != 1 {
                return Err(singleton_input_row_count_error(
                    node.as_str(),
                    alias.as_str(),
                    mat.inline_row_count(),
                    "staged expression rendering",
                ));
            }
            materialized_input_row_from_mat(
                node,
                mat,
                crate::plasm_plan::InputCardinalityProof::RuntimeCheckedSingleton,
            )?
        };
        out.insert(alias, input_row);
    }
    Ok(out)
}
