//! Recursive result contracts derived from the ordinary scoped plan.
use crate::{execute_session::ExecuteSession, plasm_plan::ValidatedPlanNode};
use plasm_core::plasm_monad::*;
use plasm_core::value_contract::{ValueContract, ValueShape};
use std::collections::BTreeMap;
mod refinements;

pub(crate) fn output_schema(
    es: &ExecuteSession,
    body: &CorrelatedBody,
) -> Result<SyntheticResultSchema, String> {
    let schema = body_result_schema(es, body)?;
    if matches!(
        body.output,
        ScopedOutput::Filter | ScopedOutput::Quantify { .. }
    ) {
        let valid = schema.fields.as_slice();
        if !matches!(valid, [field] if field.name.as_str() == "predicate" && field.value_type.as_ref().is_some_and(|t| !t.nullable && t.summary() == SyntheticValueKind::Boolean))
        {
            return Err("predicate scope must return one non-null Boolean".into());
        }
        if matches!(body.output, ScopedOutput::Filter) {
            let mut selected = body
                .parent_schema
                .clone()
                .ok_or("predicate parent schema missing")?;
            refinements::refine(body, &mut selected)?;
            return Ok(selected);
        }
    }
    Ok(schema)
}

pub(crate) fn body_result_schema(
    es: &ExecuteSession,
    body: &CorrelatedBody,
) -> Result<SyntheticResultSchema, String> {
    if let ScopedOutput::Rows { schema, .. } = &body.output {
        return Ok(schema.clone());
    }
    let PlasmReturn::Step { step } = &body.body.return_ else {
        return Err("expected one map return".into());
    };
    if let Some(PlasmStepPayload::Map(output)) = body.body.steps.get(step.as_str()) {
        if matches!(output.compute.op, ComputeOp::MergeBranches { .. }) {
            return Ok(output.compute.schema.clone());
        }
    }
    let Some(PlasmStepPayload::Derive(output)) = body.body.steps.get(step.as_str()) else {
        return Err("map requires a record derivation output".into());
    };
    let plan = crate::plasm_step_convert::lift_body(body)?;
    let value_type = ValueContract::data_value(&output.derive.value, &mut |binding, path| {
        let is_item = output.derive.item_binding.as_ref().map(|b| b.as_str()) == Some(binding);
        let source = if is_item {
            output
                .derive
                .source
                .as_deref()
                .ok_or("map output source missing")?
        } else {
            binding
        };
        if !path.is_empty() {
            return crate::python_compute::source_field_kind(
                es,
                plan.nodes(),
                source,
                &path.join("."),
                0,
            );
        }
        let row = row_contract(es, plan.nodes(), source)?;
        let collection = output
            .derive
            .inputs
            .iter()
            .any(|i| i.node == source && i.cardinality == InputCardinality::Collection);
        Ok(if collection {
            ValueContract {
                shape: ValueShape::Array {
                    element: Box::new(row),
                },
                domain: None,
                nullable: false,
            }
        } else {
            row
        })
    })?;
    if !value_type.is_non_null_record() {
        return Err("map must return a record".into());
    }
    SyntheticResultSchema::for_value(value_type)
}

pub(crate) fn row_contract(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    source: &str,
) -> Result<ValueContract, String> {
    row_contract_at(es, nodes, source, 0)
}

/// Row operators may explicitly select observed relation slots. Capturing an
/// entity as a value still excludes implicit navigation capabilities.
pub(crate) fn row_operation_contract(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    source: &str,
) -> Result<ValueContract, String> {
    record_contract_at(es, nodes, source, 0, RecordUse::RowOperation)
}

#[derive(Clone, Copy)]
enum RecordUse {
    Value,
    RowOperation,
}

pub(crate) fn row_contract_at(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    source: &str,
    depth: usize,
) -> Result<ValueContract, String> {
    record_contract_at(es, nodes, source, depth, RecordUse::Value)
}

fn record_contract_at(
    es: &ExecuteSession,
    nodes: &[ValidatedPlanNode],
    source: &str,
    depth: usize,
    purpose: RecordUse,
) -> Result<ValueContract, String> {
    if depth > 256 {
        return Err("value contract dependency depth exceeded".into());
    }
    use ValidatedPlanNode as N;
    let node = nodes
        .iter()
        .find(|n| n.id().as_str() == source)
        .ok_or("record source missing")?;
    let schema = match node {
        N::Data(d) => {
            return ValueContract::data_value(&d.data, &mut |_, _| {
                Err("data value has unresolved input".into())
            })
        }
        N::Derive(d) => {
            return ValueContract::data_value(&d.value, &mut |binding, path| {
                let source = if binding == d.item_binding.as_str() {
                    d.source.as_str()
                } else {
                    binding
                };
                if !path.is_empty() {
                    return crate::python_compute::source_field_kind(
                        es,
                        nodes,
                        source,
                        &path.join("."),
                        depth + 1,
                    );
                }
                let value = row_contract_at(es, nodes, source, depth + 1)?;
                Ok(
                    if d.inputs.iter().any(|i| {
                        i.node.as_str() == source
                            && i.proof == crate::plasm_plan::InputCardinalityProof::Collection
                    }) {
                        ValueContract {
                            shape: ValueShape::Array {
                                element: Box::new(value),
                            },
                            domain: None,
                            nullable: false,
                        }
                    } else {
                        value
                    },
                )
            })
        }
        // Iteration re-observes the seed receiver; it does not create a new domain.
        N::IterateUntil(iteration) => {
            return record_contract_at(es, nodes, iteration.source.as_str(), depth + 1, purpose)
        }
        N::MapBody(map) => Some(output_schema(es, &map.body)?),
        N::Compute(c) => match &c.compute.op {
            ComputeOp::Python { output_type, .. } => return Ok(output_type.clone()),
            ComputeOp::Limit { .. }
            | ComputeOp::Filter { .. }
            | ComputeOp::Sort { .. }
            | ComputeOp::DedupeBy { .. }
                if matches!(purpose, RecordUse::Value) =>
            {
                // Row-preserving operators retain navigation for subsequent DAG
                // operations, but capturing their values must not invent fields
                // for relations that have never been materialized.
                let source = row_contract_at(es, nodes, c.compute.source.as_str(), depth + 1)?;
                let fields = match source.shape {
                    ValueShape::Record { fields } | ValueShape::ObservedRecord { fields, .. } => {
                        fields
                    }
                    _ => return Err("row-preserving source has no record contract".into()),
                };
                let mut schema = c.compute.schema.clone();
                schema
                    .fields
                    .retain(|field| fields.contains_key(field.name.as_str()));
                schema
                    .optional_fields
                    .retain(|field| fields.contains_key(field));
                Some(schema)
            }
            _ => Some(c.compute.schema.clone()),
        },
        N::Capture(c) => {
            if let Some(value) = &c.value_contract {
                return Ok(value.clone());
            }
            c.schema.clone()
        }
        n if n.result_shape() == ResultShape::SideEffectAck => {
            return Ok(ValueContract::record(
                BTreeMap::from([
                    (
                        "completed".into(),
                        ValueContract::scalar(plasm_core::FieldType::Integer),
                    ),
                    (
                        "failed".into(),
                        ValueContract::scalar(plasm_core::FieldType::Integer),
                    ),
                ]),
                Default::default(),
            ))
        }
        N::Surface(_) | N::RelationTraversal(_) | N::ForEach(_) => None,
    };
    if let Some(schema) = schema {
        return schema.row_contract();
    }
    let owner = match node {
        N::Surface(s) => s.qualified_entity.as_ref().ok_or("record owner missing")?,
        N::RelationTraversal(r) => &r.relation.target,
        N::ForEach(f) => &f.effect_template.qualified_entity,
        N::Capture(c) => &c.entity,
        _ => return Err("record source has no recursive schema".into()),
    };
    let cgs =
        crate::catalog_ownership::resolve_cgs_for_entry_entity(es, &owner.entry_id, &owner.entity)?;
    let entity = cgs
        .get_entity(&owner.entity)
        .ok_or("record entity missing")?;
    let projection = match node {
        N::Surface(s) => (!s.projection.is_empty()).then_some(s.projection.as_slice()),
        N::RelationTraversal(r) => r.relation.ir.projection.as_deref(),
        N::ForEach(f) => {
            if !f.projection.is_empty() {
                Some(f.projection.as_slice())
            } else {
                (!f.effect_template.projection.is_empty())
                    .then_some(f.effect_template.projection.as_slice())
            }
        }
        _ => None,
    };
    let names: Vec<&str> = match projection {
        Some(fields) => fields.iter().map(String::as_str).collect(),
        None if matches!(purpose, RecordUse::RowOperation) => entity
            .fields
            .keys()
            .map(|name| name.as_str())
            .chain(entity.relations.keys().map(|name| name.as_str()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect(),
        None => entity.fields.keys().map(|name| name.as_str()).collect(),
    };
    let optional_fields = if projection.is_none() {
        names.iter().map(|name| name.to_string()).collect()
    } else {
        Default::default()
    };
    let fields = names
        .into_iter()
        .map(|name| {
            Ok((
                name.to_string(),
                crate::python_compute::source_field_kind(es, nodes, source, name, depth + 1)?,
            ))
        })
        .collect::<Result<_, String>>()?;
    Ok(ValueContract::record(fields, optional_fields))
}
