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
            return Err(at(site, "compute requires one explicit rowset dependency"));
        }
        let code = self
            .methods
            .get(method)
            .ok_or_else(|| at(site, "unknown compute method"))?
            .clone();
        let source = self.expr(&call.arguments.args[0], None)?;
        let owner = super::super::schema_validate::resolve_qualified_entity_for_dag_source(
            &self.state,
            &[],
            source.clone(),
        )
        .ok_or("compute input has no catalog ownership")?;
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            self.es,
            &owner.entry_id,
            &owner.entity,
        )?;
        let (inferred, per_row) = crate::python_compute::input_mode(&code)?;
        let input_schema = if inferred {
            Some(inferred_schema(self.es, &self.state, &source, 0)?)
        } else {
            None
        };
        let symbols = self.state.sym_map_for(self.es);
        let token = symbols.entity_sym_for(&owner.entry_id, &owner.entity);
        let checked = crate::python_compute::CheckedCompute::compile_input(
            &code,
            cgs,
            &owner.entry_id,
            symbols.as_ref(),
            input_schema.as_ref().map(|schema| (schema, token.as_str())),
            per_row,
        )?;
        if checked.contract.owner.entity.as_str() != owner.entity {
            return Err(at(site, "compute annotation does not match source entity"));
        }
        self.insert(DagNode {
            id: id.into(),
            expr: String::new(),
            singleton: !per_row,
            page_size: None,
            source: super::super::types::DagNodeSource::Compute {
                source,
                op: ComputeOp::Python {
                    source: code,
                    entry_id: owner.entry_id,
                    entity: owner.entity,
                    catalog_hash: cgs.catalog_cgs_hash_hex(),
                    contract_version: 3,
                    input_schema,
                    per_row,
                },
                schema: SyntheticResultSchema {
                    entity: None,
                    fields: vec![SyntheticFieldSchema {
                        value_type: Some(plasm_core::value_contract::ValueContract::scalar(
                            plasm_core::FieldType::String,
                        )),
                        name: OutputName::new("content")?,
                        value_kind: SyntheticValueKind::String,
                        source: None,
                    }],
                },
                collection_alias: None,
            },
        })
    }
}

fn inferred_schema(
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
    if let DagNodeSource::Derive { source, value, .. } = &node.source {
        let t =
            plasm_core::value_contract::ValueContract::data_value(value, &mut |binding, path| {
                let dependency = if state.get(binding).is_some() {
                    binding
                } else {
                    source.as_str()
                };
                let schema = inferred_schema(es, state, dependency, depth + 1)?;
                let name = path.join(".");
                schema
                    .fields
                    .iter()
                    .find(|f| f.name.as_str() == name)
                    .and_then(|f| f.value_type.clone())
                    .ok_or_else(|| format!("missing inferred field {name}"))
            })?;
        let plasm_core::value_contract::ValueShape::Record { fields } = t.shape else {
            return Err("Row requires a record-shaped derivation".into());
        };
        return Ok(SyntheticResultSchema {
            entity: None,
            fields: fields
                .into_iter()
                .map(|(name, value_type)| {
                    Ok(SyntheticFieldSchema {
                        name: OutputName::new(name)?,
                        value_kind: value_type.summary(),
                        value_type: Some(value_type),
                        source: None,
                    })
                })
                .collect::<Result<_, String>>()?,
        });
    }
    Ok(
        super::super::schema_validate::compute_passthrough_or_fallback_schema(
            es,
            state,
            &[],
            id,
            "PythonInput",
        ),
    )
}
