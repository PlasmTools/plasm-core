//! Root text computations are reviewed DAG nodes, never evaluated during planning.
use super::*;
use ruff_python_ast::ExprCall;

impl Lower<'_> {
    pub(super) fn text_compute(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        method: &str,
        id: &str,
    ) -> Result<String, String> {
        if call.arguments.args.len() != 1 || !call.arguments.keywords.is_empty() {
            return Err(at(site, "compute requires one explicit input dependency"));
        }
        let code = self
            .methods
            .get(method)
            .ok_or_else(|| at(site, "unknown compute method"))?
            .clone();
        let source = self.expr(&call.arguments.args[0], None)?;
        self.emit_python_compute(source, &code, id)
    }

    pub(super) fn emit_python_compute(
        &mut self,
        source: String,
        code: &str,
        id: &str,
    ) -> Result<String, String> {
        let op = prepare_op(self.es, &self.state, &source, code)?;
        let ComputeOp::Python { per_row, .. } = &op else {
            unreachable!()
        };
        let per_row = *per_row;
        let output = crate::python_compute::check_op(self.es, &op)?.output;
        let singleton = !per_row
            || super::super::binding_contract(&self.state, &source)
                .is_some_and(|c| c.row_cardinality.permits_scalar_field_extract());
        self.insert(DagNode {
            id: id.into(),
            expr: String::new(),
            singleton,
            page_size: None,
            source: super::super::types::DagNodeSource::Compute {
                source,
                op,
                schema: SyntheticResultSchema::for_value(output)?,
                collection_alias: None,
            },
        })
    }
}

pub(super) fn inferred_schema(
    es: &ExecuteSession,
    state: &super::super::types::CompileState<'_>,
    id: &str,
    depth: usize,
) -> Result<SyntheticResultSchema, String> {
    use super::super::types::DagNodeSource;
    if depth >= 64 {
        return Err("inferred row schema depth exceeded".into());
    }
    let node = state.get(id).ok_or("missing inferred row source")?;
    if let DagNodeSource::Derive {
        value_type: Some(schema),
        ..
    } = &node.source
    {
        return SyntheticResultSchema::for_value(schema.clone());
    }
    if let DagNodeSource::ScalarExtract { source, wire } = &node.source {
        let schema = inferred_schema(es, state, source, depth + 1)?;
        let mut field = schema
            .fields
            .into_iter()
            .find(|field| field.name.as_str() == wire)
            .ok_or_else(|| format!("missing scalar value field {wire}"))?;
        field.name = OutputName::new("value")?;
        return Ok(SyntheticResultSchema {
            optional_fields: Default::default(),
            entity: None,
            fields: vec![field],
        });
    }
    if let DagNodeSource::Data(PlanValue::Literal { value }) = &node.source {
        let value_type = plasm_core::value_contract::ValueContract::literal(value.value())?;
        return SyntheticResultSchema::for_value(value_type);
    }
    if let DagNodeSource::Derive {
        value_type: _,
        source,
        value,
        inputs,
    } = &node.source
    {
        let t = derive_contract(es, state, source, value, inputs, depth)?;
        return SyntheticResultSchema::for_value(t);
    }
    let mut schema = super::super::schema_validate::compute_passthrough_or_fallback_schema(
        es,
        state,
        &[],
        id,
        "PythonInput",
    );
    // A captured entity value contains data fields, not unexecuted navigation
    // capabilities. Apply this recursively, before constructing capture records.
    if implicit_entity_value(state, id, 0) {
        let owner = compute_owner(state, id, 0).ok_or("missing input entity owner")?;
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            es,
            &owner.entry_id,
            &owner.entity,
        )?;
        let entity = cgs
            .get_entity(&owner.entity)
            .ok_or("missing input entity")?;
        schema
            .fields
            .retain(|field| entity.fields.contains_key(field.name.as_str()));
        schema
            .optional_fields
            .retain(|field| entity.fields.contains_key(field.as_str()));
    }
    Ok(schema)
}

pub(super) fn derive_contract(
    es: &ExecuteSession,
    state: &super::super::types::CompileState<'_>,
    source: &str,
    value: &PlasmDataValue,
    inputs: &[crate::plasm_plan::PlanDataInput],
    depth: usize,
) -> Result<plasm_core::value_contract::ValueContract, String> {
    plasm_core::value_contract::ValueContract::data_value(value, &mut |binding, path| {
        let dependency = if state.get(binding).is_some() {
            binding
        } else {
            source
        };
        let schema = inferred_schema(es, state, dependency, depth + 1)?;
        if path.is_empty() {
            use plasm_core::value_contract::{ValueContract, ValueShape};
            let row = if super::super::binding_contract(state, dependency)
                .is_some_and(|c| c.value_kind == BindingValueKind::ScalarCell)
            {
                schema
                    .fields
                    .first()
                    .and_then(|f| f.value_type.clone())
                    .ok_or_else(|| "missing scalar value contract".to_string())?
            } else {
                schema.row_contract()?
            };
            return Ok(
                if inputs.iter().any(|i| {
                    i.node == dependency
                        && i.cardinality == crate::plasm_plan::InputCardinality::Collection
                }) {
                    ValueContract {
                        shape: ValueShape::Array {
                            element: Box::new(row),
                        },
                        domain: None,
                        nullable: false,
                    }
                } else {
                    row
                },
            );
        }
        let mut value = schema.row_contract()?;
        for name in path {
            value = value.field(name)?;
        }
        Ok(value)
    })
}

/// Shared recursive input admission for root and scoped row computations.
pub(super) fn prepare_op(
    es: &ExecuteSession,
    state: &super::super::types::CompileState<'_>,
    source: &str,
    code: &str,
) -> Result<ComputeOp, String> {
    let owner = compute_owner(state, source, 0);
    let entry = if let Some(owner) = &owner {
        owner.entry_id.clone()
    } else {
        es.contexts_by_entry
            .keys()
            .min()
            .cloned()
            .ok_or("compute requires a pinned session context")?
    };
    let cgs = es
        .contexts_by_entry
        .get(&entry)
        .ok_or("compute context is not loaded")?
        .cgs
        .as_ref();
    let schema = inferred_schema(es, state, source, 0)?;
    let input_schema = Some(schema);
    let symbols = state.sym_map_for(es);
    let token = owner
        .as_ref()
        .map(|owner| symbols.entity_sym_for(&owner.entry_id, &owner.entity))
        .unwrap_or_default();
    let checked = crate::python_compute::PreparedCompute::prepare_typed(
        code,
        cgs,
        &entry,
        symbols.as_ref(),
        input_schema.as_ref().map(|schema| (schema, token.as_str())),
        &crate::python_compute::return_domains(es)?,
    )?;
    if checked.contract.as_ref().map(|c| c.owner.entity.as_str())
        != owner.as_ref().map(|o| o.entity.as_str())
    {
        return Err("compute annotation does not match source entity".to_string());
    }
    Ok(ComputeOp::Python {
        source: code.to_owned(),
        entry_id: entry,
        entity: owner.map(|owner| owner.entity),
        catalog_hash: cgs.catalog_cgs_hash_hex(),
        contract_version: 9,
        language_profile: crate::python_compute::LANGUAGE_PROFILE.into(),
        input_schema,
        output_type: checked.output,
        per_row: checked.per_row,
    })
}

/// A constructed value's dependencies supply its checking context, not entity
/// receiver authority. Keep this separate from catalog navigation resolution.
fn compute_owner(
    state: &super::super::types::CompileState<'_>,
    source: &str,
    depth: usize,
) -> Option<QualifiedEntityKey> {
    if depth >= 64 {
        return None;
    }
    if let Some(owner) = super::super::schema_validate::resolve_qualified_entity_for_dag_source(
        state,
        &[],
        source.to_string(),
    ) {
        return Some(owner);
    }
    match &state.get(source)?.source {
        super::super::types::DagNodeSource::Derive { inputs, .. } => inputs
            .iter()
            .find_map(|input| compute_owner(state, &input.node, depth + 1)),
        super::super::types::DagNodeSource::ScalarExtract { source, .. }
        | super::super::types::DagNodeSource::Compute { source, .. } => {
            compute_owner(state, source, depth + 1)
        }
        _ => None,
    }
}

fn implicit_entity_value(
    state: &super::super::types::CompileState<'_>,
    source: &str,
    depth: usize,
) -> bool {
    use super::super::types::DagNodeSource;
    if depth >= 64 {
        return false;
    }
    match state.get(source).map(|node| &node.source) {
        Some(DagNodeSource::Surface { .. } | DagNodeSource::RelationTraversal { .. }) => true,
        Some(DagNodeSource::MapBody { body, .. }) => matches!(
            body.output,
            ScopedOutput::Rows {
                entity_authority: true,
                ..
            }
        ),
        Some(DagNodeSource::Compute {
            source,
            op:
                ComputeOp::Limit { .. }
                | ComputeOp::Filter { .. }
                | ComputeOp::Sort { .. }
                | ComputeOp::DedupeBy { .. },
            ..
        }) => implicit_entity_value(state, source, depth + 1),
        _ => false,
    }
}
