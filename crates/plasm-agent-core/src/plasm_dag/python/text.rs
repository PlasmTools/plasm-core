//! Root text computations are reviewed DAG nodes, never evaluated during planning.
use super::*;
use crate::program_rejection::PythonLoweringInvariantError;

use ruff_python_ast::ExprCall;

impl Lower<'_> {
    pub(super) fn text_compute(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        method: &str,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        let code = self
            .methods
            .get(method)
            .ok_or_else(|| {
                at(
                    site,
                    PythonSourceError::UnknownComputeMethod {
                        method: method.to_owned(),
                    },
                )
            })?
            .clone();
        let lowered = self.text_compute_source(site, call, code, id)?;
        self.used_methods.insert(method.to_owned());
        Ok(lowered)
    }

    pub(super) fn text_compute_source(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        code: String,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
        let parsed = ruff_python_parser::parse_module(&code).map_err(|error| {
            PythonLoweringError::Source {
                error: std::sync::Arc::new(crate::program_rejection::PythonSourceError::Parse {
                    source: error,
                }),
                span: None,
            }
        })?;
        let Some(Stmt::FunctionDef(def)) = parsed.suite().last() else {
            return Err(
                crate::program_rejection::PythonComputeError::ComputeDefinitionMissing.into(),
            );
        };
        let mut def = def.clone();
        let mut call = call.clone();
        if def.parameters.posonlyargs.is_empty()
            && def.parameters.args.is_empty()
            && def.parameters.kwonlyargs.is_empty()
        {
            // The compute wire ABI needs an input even when the Python method
            // does not. Use the same private unit port for declared computes
            // and pure helpers; it is not a user-visible dependency.
            let unit = ruff_python_parser::parse_module("def unit(__plasm_unit: int):\n    pass\n")
                .map_err(|error| PythonLoweringError::Source {
                    error: std::sync::Arc::new(
                        crate::program_rejection::PythonSourceError::Parse { source: error },
                    ),
                    span: None,
                })?;
            let Some(Stmt::FunctionDef(unit)) = unit.suite().first() else {
                unreachable!()
            };
            def.parameters.args.push(unit.parameters.args[0].clone());
            call.arguments.args.push(
                *ruff_python_parser::parse_expression("0")
                    .map_err(|error| PythonLoweringError::Source {
                        error: std::sync::Arc::new(
                            crate::program_rejection::PythonSourceError::Parse { source: error },
                        ),
                        span: None,
                    })?
                    .into_syntax()
                    .body,
            );
        }
        let mut expressions: Vec<_> = call
            .arguments
            .args
            .iter()
            .map(|expr| (None, expr))
            .collect();
        for keyword in &call.arguments.keywords {
            let name = keyword
                .arg
                .as_ref()
                .ok_or_else(|| at(site, PythonSourceError::ExpandedComputeKeywords))?;
            expressions.push((Some(name.to_string()), &keyword.value));
        }
        let shape = expressions
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let mut callable = def.clone();
        // @compute is a host declaration marker, not a Python wrapper callable.
        callable.decorator_list.clear();
        let bindings = monty_analysis::bind_arguments(
            &monty::statement_source(&Stmt::FunctionDef(callable)),
            &shape,
        )?;
        // Binding precedes normalization: the wire ABI has required named ports,
        // while Python owns positional-only, keyword-only and default semantics.
        let parameters = def
            .parameters
            .posonlyargs
            .iter()
            .chain(&def.parameters.args)
            .chain(&def.parameters.kwonlyargs)
            .collect::<Vec<_>>();
        let mut bound = bindings
            .iter()
            .zip(&expressions)
            .map(|(binding, (_, expression))| (binding.parameter.clone(), *expression))
            .collect::<Vec<_>>();
        for parameter in &parameters {
            if !bound
                .iter()
                .any(|(name, _)| name == parameter.parameter.name.as_str())
            {
                let default = parameter
                    .default
                    .as_deref()
                    .ok_or(crate::program_rejection::PythonComputeError::BoundInputMissing)?;
                bound.push((parameter.parameter.name.to_string(), default));
            }
        }
        let parameters = def
            .parameters
            .posonlyargs
            .iter()
            .chain(&def.parameters.args)
            .chain(&def.parameters.kwonlyargs)
            .collect::<Vec<_>>();
        let source = if parameters.len() == 1 {
            self.expr(bound[0].1, None)?
        } else {
            let mut inputs = BTreeMap::new();
            let mut fields = BTreeMap::new();
            for (name, expression) in &bound {
                let parameter = parameters
                    .iter()
                    .find(|parameter| parameter.parameter.name.as_str() == name)
                    .ok_or(
                        crate::program_rejection::PythonComputeError::MaterializationPortMissing,
                    )?;
                let mut argument_inputs = BTreeMap::new();
                let value = self.scoped_value(expression, &mut argument_inputs)?;
                let annotation = parameter.parameter.annotation.as_deref();
                if let PlasmDataValue::NodeSymbol { node, path, .. } = &value {
                    if path.is_empty() {
                        let singleton = super::super::binding_contract(&self.state, node)
                            .is_some_and(|c| c.row_cardinality.permits_scalar_field_extract());
                        if annotation.is_some_and(crate::python_compute::is_row_annotation)
                            && !singleton
                        {
                            return Err(at(
                                expression,
                                PythonSourceError::RowComputeNeedsSingleton,
                            ));
                        }
                        if singleton {
                            argument_inputs
                                .get_mut(node)
                                .ok_or(crate::program_rejection::PythonComputeError::ComputeRowInputMissing)?
                                .cardinality = crate::plasm_plan::InputCardinality::Singleton;
                        }
                    }
                }
                // Give each argument its own value port, including repeated use
                // of one source at different cardinalities.
                let argument = self.fresh();
                // A captured entity is a nested value, not the identity of the
                // synthetic argument node. Keep its metadata under a value port.
                self.emit_value(
                    PlasmDataValue::Object {
                        fields: BTreeMap::from([("argument".into(), value)]),
                    },
                    argument_inputs.into_values().collect(),
                    &argument,
                )?;
                inputs.insert(
                    argument.clone(),
                    crate::plasm_plan::PlanDataInput {
                        node: argument.clone(),
                        alias: argument.clone(),
                        cardinality: crate::plasm_plan::InputCardinality::Singleton,
                    },
                );
                fields.insert(
                    parameter.parameter.name.to_string(),
                    PlasmDataValue::NodeSymbol {
                        node: argument.clone(),
                        alias: argument,
                        path: vec!["argument".into()],
                    },
                );
            }
            let packet = self.fresh();
            self.emit_value(
                PlasmDataValue::Object { fields },
                inputs.into_values().collect(),
                &packet,
            )?;
            packet
        };
        let parameter_count = parameters.len();
        drop(parameters);
        let input = inferred_schema(self.es, &self.state, &source, 0)?
            .row_contract()
            .map_err(PythonLoweringError::from)?;
        let needs_inference = def
            .parameters
            .posonlyargs
            .iter()
            .chain(&def.parameters.args)
            .chain(&def.parameters.kwonlyargs)
            .any(|parameter| parameter.parameter.annotation.is_none());
        let single_contract =
            if parameter_count == 1 && needs_inference {
                Some(super::super::binding_contract(&self.state, &source).ok_or(
                    crate::program_rejection::PythonComputeError::ComputeInputContractMissing,
                )?)
            } else {
                None
            };
        for parameter in def
            .parameters
            .posonlyargs
            .iter_mut()
            .chain(&mut def.parameters.args)
            .chain(&mut def.parameters.kwonlyargs)
        {
            if parameter.parameter.annotation.is_some() {
                continue;
            }
            let value = if parameter_count == 1 {
                input.clone()
            } else {
                // The typed packet preserves each dependency separately;
                // inference never joins independent collections.
                input.field(parameter.parameter.name.as_str())?
            };
            let scalar_cell = single_contract
                .as_ref()
                .is_some_and(|contract| contract.value_kind == BindingValueKind::ScalarCell);
            let collection = single_contract
                .as_ref()
                .is_some_and(|contract| !contract.row_cardinality.permits_scalar_field_extract());
            let annotation = crate::python_compute::inferred_helper_input_annotation(
                &value,
                scalar_cell,
                collection,
            )?;
            parameter.parameter.annotation = Some(
                ruff_python_parser::parse_expression(&annotation)
                    .map_err(|error| PythonLoweringError::Source {
                        error: std::sync::Arc::new(
                            crate::program_rejection::PythonSourceError::Parse { source: error },
                        ),
                        span: None,
                    })?
                    .into_syntax()
                    .body,
            );
        }
        let parameters = def
            .parameters
            .posonlyargs
            .iter()
            .chain(&def.parameters.args)
            .chain(&def.parameters.kwonlyargs)
            .collect::<Vec<_>>();
        for parameter in &parameters {
            if let Some(default) = parameter.default.as_deref() {
                let annotation = parameter
                    .parameter
                    .annotation
                    .as_deref()
                .ok_or(crate::program_rejection::PythonComputeError::InferredComputeInputContractMissing)?;
                let argument = if parameters.len() == 1 {
                    input.clone()
                } else {
                    input.field(parameter.parameter.name.as_str())?
                };
                crate::python_compute::check_callback_closed_return(
                    self.es,
                    annotation,
                    &argument,
                    default,
                    &self.imports.source,
                )?;
            }
        }
        let mut normalized = def.clone();
        normalized.parameters.args = parameters
            .iter()
            .map(|parameter| {
                let mut parameter = (*parameter).clone();
                parameter.default = None;
                parameter
            })
            .collect();
        normalized.parameters.posonlyargs.clear();
        normalized.parameters.kwonlyargs.clear();
        let code = admission::method_source(&self.imports.source, &code, &def, normalized)?;
        self.emit_python_compute(source, &code, id)
    }

    pub(super) fn emit_python_compute(
        &mut self,
        source: String,
        code: &str,
        id: &str,
    ) -> Result<String, PythonLoweringError> {
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
) -> Result<SyntheticResultSchema, PythonLoweringError> {
    use super::super::types::DagNodeSource;
    if depth >= 64 {
        return Err(PythonLoweringInvariantError::InferredSchemaDepthExceeded.into());
    }
    let node = state
        .get(id)
        .ok_or(PythonLoweringInvariantError::MissingInferredSourceNode)?;
    if let DagNodeSource::Derive {
        value_type: Some(schema),
        ..
    } = &node.source
    {
        return SyntheticResultSchema::for_value(schema.clone()).map_err(Into::into);
    }
    if let DagNodeSource::ScalarExtract { source, wire } = &node.source {
        let schema = inferred_schema(es, state, source, depth + 1)?;
        let mut field = schema
            .fields
            .into_iter()
            .find(|field| field.name.as_str() == wire)
            .ok_or_else(|| PythonLoweringInvariantError::ScalarValueFieldMissing {
                field: wire.clone(),
            })?;
        field.name = OutputName::new("value")?;
        return Ok(SyntheticResultSchema {
            optional_fields: Default::default(),
            entity: None,
            fields: vec![field],
        });
    }
    if let DagNodeSource::Data(PlanValue::Literal { value }) = &node.source {
        let value_type = plasm_core::value_contract::ValueContract::literal(value.value())?;
        return SyntheticResultSchema::for_value(value_type).map_err(Into::into);
    }
    if let DagNodeSource::Derive {
        value_type: _,
        source,
        value,
        inputs,
    } = &node.source
    {
        let t = derive_contract(es, state, source, value, inputs, depth)?;
        return SyntheticResultSchema::for_value(t).map_err(Into::into);
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
        let owner = compute_owner(state, id, 0)
            .ok_or(PythonLoweringInvariantError::InputEntityOwnerMissing)?;
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            es,
            &owner.entry_id,
            &owner.entity,
        )?;
        let entity = cgs
            .get_entity(&owner.entity)
            .ok_or(PythonLoweringInvariantError::InputEntityMissing)?;
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
) -> Result<plasm_core::value_contract::ValueContract, PythonLoweringError> {
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
                    .ok_or(PythonLoweringInvariantError::ScopedOutputContractMissing)?
            } else {
                schema.row_contract()?
            };
            return Ok::<_, PythonLoweringError>(
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
            value = value.field(name).map_err(PythonLoweringError::from)?;
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
) -> Result<ComputeOp, PythonLoweringError> {
    let parsed =
        ruff_python_parser::parse_module(code).map_err(|error| PythonLoweringError::Source {
            error: std::sync::Arc::new(crate::program_rejection::PythonSourceError::Parse {
                source: error,
            }),
            span: None,
        })?;
    let multiple = matches!(parsed.suite().last(), Some(Stmt::FunctionDef(def)) if def.parameters.args.len() > 1);
    let owner = if multiple {
        None
    } else {
        compute_owner(state, source, 0)
    };
    let entry = if let Some(owner) = &owner {
        owner.entry_id.clone()
    } else {
        es.contexts_by_entry
            .keys()
            .min()
            .cloned()
            .ok_or(PythonLoweringInvariantError::PinnedSessionContextMissing)?
    };
    let cgs = es
        .contexts_by_entry
        .get(&entry)
        .ok_or(PythonLoweringInvariantError::ComputeContextMissing)?
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
        if multiple
            || super::super::binding_contract(state, source)
                .is_some_and(|contract| contract.row_cardinality.permits_scalar_field_extract())
        {
            crate::python_compute::ComputeInputMode::Singleton
        } else {
            crate::python_compute::ComputeInputMode::Collection
        },
    )?;
    if checked.contract.as_ref().map(|c| c.owner.entity.as_str())
        != owner.as_ref().map(|o| o.entity.as_str())
    {
        return Err(crate::program_rejection::PythonComputeError::AnnotationSourceMismatch.into());
    }
    Ok(ComputeOp::Python {
        source: code.to_owned(),
        entry_id: entry,
        entity: owner.map(|owner| owner.entity),
        catalog_hash: cgs.catalog_cgs_hash_hex(),
        contract_version: crate::python_compute::CONTRACT_VERSION,
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
