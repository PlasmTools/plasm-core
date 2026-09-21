use std::collections::BTreeMap;

use plasm_runtime::{eval_compute_ops, ComputeEvalOutcome};

use crate::plasm_plan::OutputName;
use crate::plasm_render_compile::render_context_hint;

use super::super::*;

pub(crate) async fn eval_compute_with_row_source(
    compute: &ComputeTemplate,
    row_source: &MaterializedRowSource,
    cross_binding_rows: &BTreeMap<String, Vec<serde_json::Value>>,
    es: &ExecuteSession,
    st: &PlasmHostState,
    session_id: &str,
    cgs: &CGS,
) -> Result<Vec<serde_json::Value>, String> {
    let cap = if matches!(&compute.op, ComputeOp::Render { .. }) {
        Some(crate::plasm_plan::PLAN_RENDER_MAX_ROWS)
    } else {
        None
    };
    let rows = crate::graph_rehydrate::GraphSurfaceRehydrator::new(es, st, session_id, cgs)
        .resolve_row_source_rows(row_source, cap)
        .await?;
    eval_compute_from_rows(compute, &rows, cross_binding_rows)
}

pub(crate) fn eval_compute_from_rows(
    compute: &ComputeTemplate,
    rows: &[serde_json::Value],
    cross_binding_rows: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<Vec<serde_json::Value>, String> {
    if let ComputeOp::Union { other } = &compute.op {
        let right = cross_binding_rows.get(other.as_str()).ok_or_else(|| {
            format!(
                "union RHS `{}` has not been materialized (RA-14)",
                other.as_str()
            )
        })?;
        return plasm_core::row_contract::PublicRowSchema::new(&compute.schema).union(rows, right);
    }
    let op = bind_filter_op(&compute.op, cross_binding_rows)?;
    match eval_compute_ops(std::slice::from_ref(&op), rows)? {
        ComputeEvalOutcome::Rows(out) => Ok(out),
        ComputeEvalOutcome::Render {
            rows,
            columns,
            column_aliases,
            template,
            collection_alias,
            render_bindings,
        } => render_compute(&RenderComputeInput {
            primary_rows: &rows,
            columns: &RenderColumns::from_op_parts(columns, column_aliases),
            template: &template,
            collection_alias: collection_alias
                .as_ref()
                .or(compute.collection_alias.as_ref()),
            render_bindings: &render_bindings,
            binding_rows: cross_binding_rows,
        }),
    }
}

pub(crate) struct RenderComputeInput<'a> {
    pub primary_rows: &'a [serde_json::Value],
    pub columns: &'a RenderColumns,
    pub template: &'a str,
    pub collection_alias: Option<&'a OutputName>,
    pub render_bindings: &'a [OutputName],
    pub binding_rows: &'a BTreeMap<String, Vec<serde_json::Value>>,
}

fn effective_render_binding_labels(
    render_bindings: &[OutputName],
    collection_alias: Option<&OutputName>,
) -> Vec<String> {
    if !render_bindings.is_empty() {
        render_bindings
            .iter()
            .map(|label| label.as_str().to_string())
            .collect()
    } else if let Some(alias) = collection_alias {
        vec![alias.as_str().to_string()]
    } else {
        vec![]
    }
}

pub(crate) fn render_compute(
    input: &RenderComputeInput<'_>,
) -> Result<Vec<serde_json::Value>, String> {
    let rows = input.primary_rows;
    if rows.len() > PLAN_RENDER_MAX_ROWS {
        return Err(format!(
            "Plan.render source has {} rows; use Plan.limit(...) to stay at or below {PLAN_RENDER_MAX_ROWS}",
            rows.len()
        ));
    }
    let source_label = input
        .collection_alias
        .map(|a| a.as_str())
        .unwrap_or("source");
    let binding_labels =
        effective_render_binding_labels(input.render_bindings, input.collection_alias);
    let mut named_bindings = BTreeMap::new();
    for label in &binding_labels {
        let binding_rows = input
            .binding_rows
            .get(label.as_str())
            .map(|rows| rows.as_slice())
            .unwrap_or(rows);
        named_bindings.insert(label.clone(), binding_rows.to_vec());
    }

    let mut out = Vec::with_capacity(rows.len());
    for (row_index, row) in rows.iter().enumerate() {
        let projected = if input.columns.is_empty() {
            row.clone()
        } else {
            serde_json::Value::Object(input.columns.project_row(row, row_index)?)
        };
        let rendered = plasm_core::render_minijinja(input.template, Some(&projected), &named_bindings)
            .map_err(|e| {
                let hint = render_context_hint(input.columns, Some(source_label));
                format!(
                    "template render failed on binding `{source_label}` at row {row_index}: {e}. {hint}"
                )
            })?;
        if rendered.chars().count() > PLAN_RENDER_MAX_OUTPUT_CHARS {
            return Err(format!(
                "template render failed on binding `{source_label}` at row {row_index}: output exceeds {PLAN_RENDER_MAX_OUTPUT_CHARS} characters"
            ));
        }
        out.push(serde_json::json!({ "content": rendered }));
    }
    Ok(out)
}

pub(crate) fn binding_rows_for_compute(
    compute: &ComputeTemplate,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<BTreeMap<String, Vec<serde_json::Value>>, String> {
    let mut out = binding_rows_for_render(compute, materialized)?;
    for label in compute_binding_labels(&compute.op) {
        if out.contains_key(&label) {
            continue;
        }
        let node_id = PlanNodeId::new(label.clone())?;
        let mat = materialized.get(&node_id).ok_or_else(|| {
            format!("membership binding `{label}`: node `{label}` has not been materialized")
        })?;
        let rows = mat
            .row_source
            .inline_rows()
            .map(|r| r.to_vec())
            .ok_or_else(|| {
                format!("membership binding `{label}`: node `{label}` is not inline materialized")
            })?;
        out.insert(label, rows);
    }
    Ok(out)
}

pub(crate) fn resolve_filter_predicates_with_materialized(
    predicates: &plasm_core::BooleanExpr<crate::plasm_plan::PlanPredicate>,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<plasm_core::BooleanExpr<crate::plasm_plan::PlanPredicate>, String> {
    let mut binding_rows = BTreeMap::new();
    for label in compute_binding_labels(&ComputeOp::Filter {
        predicates: predicates.clone(),
    }) {
        let node_id = PlanNodeId::new(label.clone())?;
        let mat = materialized.get(&node_id).ok_or_else(|| {
            format!("membership binding `{label}`: node `{label}` has not been materialized")
        })?;
        let rows = mat
            .row_source
            .inline_rows()
            .map(|r| r.to_vec())
            .ok_or_else(|| {
                format!("membership binding `{label}`: node `{label}` is not inline materialized")
            })?;
        binding_rows.insert(label, rows);
    }
    predicates.try_map(&mut |p| bind_filter_predicate(p, &binding_rows))
}

fn bind_filter_op(
    op: &ComputeOp,
    binding_rows: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<ComputeOp, String> {
    let ComputeOp::Filter { predicates } = op else {
        return Ok(op.clone());
    };
    let resolved = predicates.try_map(&mut |pred| bind_filter_predicate(pred, binding_rows))?;
    Ok(ComputeOp::Filter {
        predicates: resolved,
    })
}

fn bind_filter_predicate(
    pred: &crate::plasm_plan::PlanPredicate,
    binding_rows: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<crate::plasm_plan::PlanPredicate, String> {
    use plasm_core::operand_binding::{BindOperands, ResolvedValue};
    if let crate::plasm_plan::PlanValue::BindingSymbol { binding, path } = &pred.value {
        if matches!(
            pred.op,
            crate::plasm_plan::PlanPredicateOp::In | crate::plasm_plan::PlanPredicateOp::NotIn
        ) {
            let rows = binding_rows.get(binding).ok_or_else(|| {
                format!("membership RHS `{binding}` has not been materialized (RA-13)")
            })?;
            let values = collect_membership_column(binding, path, rows)?;
            return Ok(crate::plasm_plan::PlanPredicate {
                field_path: pred.field_path.clone(),
                op: pred.op,
                value: crate::plasm_plan::PlanValue::Literal {
                    value: ResolvedValue::from_wire(serde_json::Value::Array(values))?,
                },
            });
        }
    }
    pred.bind_operands(&mut PredicateOperands { rows: binding_rows })
}

struct PredicateOperands<'a> {
    rows: &'a BTreeMap<String, Vec<serde_json::Value>>,
}
impl PredicateOperands<'_> {
    fn singleton(&self, node: &str) -> Result<&serde_json::Value, String> {
        let rows = self
            .rows
            .get(node)
            .ok_or_else(|| format!("predicate operand `{node}` has not been materialized"))?;
        match rows.as_slice() {
            [row] => Ok(row),
            [] => Err(format!(
                "scalar predicate operand `{node}` has zero rows; expected exactly one"
            )),
            _ => Err(format!(
                "scalar predicate operand `{node}` has {} rows; expected exactly one",
                rows.len()
            )),
        }
    }
}
impl plasm_core::operand_binding::OperandResolver for PredicateOperands<'_> {
    type Error = String;
    fn resolve(
        &mut self,
        reference: &plasm_core::PlasmInputRef,
    ) -> Result<plasm_core::operand_binding::ResolvedValue, String> {
        let (node, path) = match reference {
            plasm_core::PlasmInputRef::NodeInput { node, path } => (node, path),
            plasm_core::PlasmInputRef::RowBinding { binding, path } => (binding, path),
        };
        let mut value = self.singleton(node)?;
        for field in path {
            value = value.get(field).ok_or_else(|| {
                format!(
                    "scalar predicate operand `{node}` has no field `{}`",
                    path.join(".")
                )
            })?;
        }
        plasm_core::operand_binding::ResolvedValue::from_wire(value.clone())
    }
    fn identity(
        &mut self,
        _: plasm_core::operand_binding::IdentityTarget<'_>,
        _: &plasm_core::PlasmInputRef,
    ) -> Result<plasm_core::EntityId, String> {
        Err("identity is not a scalar predicate operand".into())
    }
    fn string(
        &mut self,
        template: &plasm_core::program_string_template::CompiledProgramString,
    ) -> Result<String, String> {
        self.template(template, &[])
    }
    fn template(
        &mut self,
        template: &plasm_core::program_string_template::CompiledProgramString,
        bindings: &[plasm_core::PlanInputBinding],
    ) -> Result<String, String> {
        let mut named = BTreeMap::new();
        for root in template.roots() {
            let node = bindings
                .iter()
                .find(|b| &b.to == root)
                .map(|b| b.from.as_str())
                .unwrap_or(root);
            named.insert(root.clone(), vec![self.singleton(node)?.clone()]);
        }
        template
            .render_minijinja_context(&plasm_core::unified_template_context(None, &named))
            .map_err(|e| e.to_string())
    }
}

fn collect_membership_column(
    binding: &str,
    path: &[String],
    rows: &[serde_json::Value],
) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for row in rows {
        let cell = membership_cell(binding, path, row)?;
        if cell.is_null() {
            continue;
        }
        let key = serde_json::to_string(&cell).map_err(|e| format!("membership cell: {e}"))?;
        if seen.insert(key) {
            out.push(cell);
        }
    }
    Ok(out)
}

fn membership_cell(
    binding: &str,
    path: &[String],
    row: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    if path.is_empty() {
        let obj = row
            .as_object()
            .ok_or_else(|| format!("membership RHS `{binding}` row is not an object"))?;
        if obj.len() != 1 {
            return Err(format!(
                "membership RHS `{binding}` must be one column; write `({binding} | select field)` (RA-13)"
            ));
        }
        return Ok(obj
            .values()
            .next()
            .cloned()
            .unwrap_or(serde_json::Value::Null));
    }
    let mut cur = row;
    for key in path {
        cur = cur
            .get(key)
            .ok_or_else(|| format!("membership RHS `{binding}` has no column `{key}`"))?;
    }
    Ok(cur.clone())
}

pub(crate) fn binding_rows_for_render(
    compute: &ComputeTemplate,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<BTreeMap<String, Vec<serde_json::Value>>, String> {
    let ComputeOp::Render {
        render_bindings, ..
    } = &compute.op
    else {
        return Ok(BTreeMap::new());
    };
    let labels =
        effective_render_binding_labels(render_bindings, compute.collection_alias.as_ref());
    let mut out = BTreeMap::new();
    for label in labels {
        let node_id = PlanNodeId::new(label.clone())?;
        let mat = materialized.get(&node_id).ok_or_else(|| {
            format!("Plan.render binding `{label}`: node `{label}` has not been materialized")
        })?;
        let rows = mat
            .row_source
            .inline_rows()
            .map(|r| r.to_vec())
            .ok_or_else(|| {
                format!("Plan.render binding `{label}`: node `{label}` is not inline materialized")
            })?;
        out.insert(label, rows);
    }
    Ok(out)
}

pub(crate) fn binding_rows_for_data_uses(
    uses: &[crate::plasm_plan::PlanResultUse],
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<BTreeMap<String, Vec<serde_json::Value>>, String> {
    let mut out = BTreeMap::new();
    for use_ref in uses {
        let label = use_ref.r#as.as_str();
        let node_id = PlanNodeId::new(use_ref.node.clone())?;
        let mat = materialized.get(&node_id).ok_or_else(|| {
            format!(
                "plain template binding `{label}`: node `{}` has not been materialized",
                use_ref.node
            )
        })?;
        let rows = mat
            .row_source
            .inline_rows()
            .map(|r| r.to_vec())
            .ok_or_else(|| {
                format!(
                    "plain template binding `{label}`: node `{}` is not inline materialized",
                    use_ref.node
                )
            })?;
        out.insert(label.to_string(), rows);
    }
    Ok(out)
}

pub(crate) fn eval_data_plan_value(
    value: &crate::plasm_plan::PlanValue,
    binding_rows: &BTreeMap<String, Vec<serde_json::Value>>,
) -> Result<Vec<serde_json::Value>, String> {
    match value {
        crate::plasm_plan::PlanValue::Template {
            template,
            input_bindings,
        } => {
            let mut named = BTreeMap::new();
            for binding in input_bindings {
                let rows = binding_rows.get(&binding.from).cloned().ok_or_else(|| {
                    format!(
                        "plain template binding `{}` has not been materialized",
                        binding.from
                    )
                })?;
                named.insert(binding.to.clone(), rows);
            }
            let rendered = template
                .render_minijinja_context(&plasm_core::unified_template_context(None, &named))
                .map_err(|e| e.to_string())?;
            Ok(vec![serde_json::Value::String(rendered)])
        }
        other => plan_value_to_rows(other),
    }
}

pub(crate) fn compute_fingerprint(node: &ValidatedPlanNode, rows: &[serde_json::Value]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(node.id().as_str().as_bytes());
    if let ValidatedPlanNode::Compute(compute) = node {
        match serde_json::to_vec(&compute.compute) {
            Ok(bytes) => hasher.update(bytes),
            Err(e) => hasher.update(format!("compute-serialization-error:{e}").as_bytes()),
        }
    }
    match serde_json::to_vec(rows) {
        Ok(bytes) => hasher.update(bytes),
        Err(e) => hasher.update(format!("rows-serialization-error:{e}").as_bytes()),
    }
    format!("plan-compute:{}", hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod scalar_operand_tests {
    use super::*;
    use crate::plasm_plan::{FieldPath, PlanPredicate, PlanPredicateOp, PlanValue};
    use serde_json::json;

    fn predicate(value: PlanValue) -> PlanPredicate {
        PlanPredicate {
            field_path: FieldPath::from_dotted("score").unwrap(),
            op: PlanPredicateOp::Eq,
            value,
        }
    }
    fn reference() -> PlanValue {
        PlanValue::NodeSymbol {
            node: "selected".into(),
            alias: "selected".into(),
            path: vec!["score".into()],
        }
    }

    #[test]
    fn scalar_predicate_missing_empty_plural_are_errors() {
        for rows in [
            vec![],
            vec![json!({})],
            vec![json!({"score":1}), json!({"score":2})],
        ] {
            let bindings = BTreeMap::from([("selected".into(), rows)]);
            assert!(bind_filter_predicate(&predicate(reference()), &bindings).is_err());
        }
        assert!(bind_filter_predicate(&predicate(reference()), &BTreeMap::new()).is_err());
    }

    #[test]
    fn scalar_template_binds_before_filtering_and_keeps_literal_text_literal() {
        let template = plasm_core::program_string_template::CompiledProgramString::compile(
            "{{ selected.score }}".into(),
        )
        .unwrap();
        let bindings = BTreeMap::from([("selected".into(), vec![json!({"score":"one.score"})])]);
        let value = PlanValue::Template {
            template,
            input_bindings: vec![],
        };
        let resolved = bind_filter_predicate(&predicate(value), &bindings).unwrap();
        assert_eq!(
            resolved.value.into_resolved().unwrap().value(),
            &plasm_core::Value::String("one.score".into())
        );
    }

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(64))]
        #[test]
        fn scalar_operand_serialization_preserves_selection(
            selected in -20i64..20,
            values in proptest::collection::vec(-20i64..20, 0..60),
        ) {
            let pred = predicate(reference());
            let wire = serde_json::to_vec(&pred).unwrap();
            let restored: PlanPredicate = serde_json::from_slice(&wire).unwrap();
            proptest::prop_assert_eq!(pred.value.dependencies(), restored.value.dependencies());
            for representation in 0..3 {
                let encode = |v: i64| match representation {
                    0 => json!(v),
                    1 => json!(v % 2 == 0),
                    _ => json!(format!("é:{{{{ literal }}}}:{v}")),
                };
                let chosen = encode(selected);
                let bindings = BTreeMap::from([("selected".into(), vec![json!({"score":chosen})])]);
                let rows = values.iter().enumerate().map(|(id,v)| json!({"id":id % 3,"score":encode(*v)})).collect::<Vec<_>>();
                let resolved = bind_filter_predicate(&restored, &bindings).unwrap();
                let op = ComputeOp::Filter { predicates: vec![resolved].into() };
                let ComputeEvalOutcome::Rows(actual) = eval_compute_ops(&[op], &rows).unwrap() else { panic!("filter returned render") };
                let expected = rows.into_iter().filter(|r| r["score"] == chosen).collect::<Vec<_>>();
                proptest::prop_assert_eq!(actual, expected);
            }
        }
    }
}
