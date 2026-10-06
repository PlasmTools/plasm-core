//! Recursive scoped lowering. Body expressions use the ordinary DAG compiler.
use super::literal_operands::LiteralOperand;
use super::*;
use crate::plasm_plan::{InputCardinality, PlanDataInput};
use crate::program_rejection::PythonLoweringInvariantError;
use std::num::NonZeroU32;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ScopeMode {
    Record,
    Rows,
    Filter,
    PredicateValue,
    Value,
    Quantify(bool),
}

impl ScopeMode {
    fn parent_budget(self) -> u32 {
        if self == Self::Rows {
            65_536
        } else {
            256
        }
    }
}

impl Lower<'_> {
    pub(super) fn map(&mut self, e: &PyExpr) -> Result<CorrelatedBody, PythonLoweringError> {
        self.scope(e, ScopeMode::Record, None)
    }

    pub(super) fn scope(
        &mut self,
        e: &PyExpr,
        mode: ScopeMode,
        source_override: Option<&str>,
    ) -> Result<CorrelatedBody, PythonLoweringError> {
        if self.frame.depth >= 16 {
            return Err(at(
                e,
                PythonSourceError::MapScopeDepth {
                    max: 16,
                    actual: self.frame.depth,
                },
            ));
        }
        let PyExpr::Call(call) = e else {
            return Err(at(e, PythonSourceError::ExpectedMapCall));
        };
        let PyExpr::Attribute(attr) = &*call.func else {
            return Err(at(e, PythonSourceError::ExpectedMapReceiver));
        };
        if call.arguments.args.len() != 1 {
            return Err(at(
                e,
                if mode == ScopeMode::Rows {
                    PythonSourceError::FlatMapCallbackCount {
                        actual: call.arguments.args.len(),
                    }
                } else {
                    PythonSourceError::MapCallbackCount {
                        actual: call.arguments.args.len(),
                    }
                },
            ));
        }
        if call.arguments.keywords.len() > 1
            || call
                .arguments
                .keywords
                .first()
                .is_some_and(|k| k.arg.as_ref().map(|s| s.as_str()) != Some("max_parents"))
        {
            return Err(at(
                e,
                if mode == ScopeMode::Rows {
                    PythonSourceError::FlatMapKeywordShape {
                        keywords: call
                            .arguments
                            .keywords
                            .iter()
                            .map(|k| k.arg.as_ref().map(ToString::to_string))
                            .collect(),
                    }
                } else {
                    PythonSourceError::MapKeywordShape {
                        keywords: call
                            .arguments
                            .keywords
                            .iter()
                            .map(|k| k.arg.as_ref().map(ToString::to_string))
                            .collect(),
                    }
                },
            ));
        }
        if mode != ScopeMode::Rows && call.arguments.keywords.is_empty() {
            return Err(at(e, PythonSourceError::MissingMapParentBound));
        }
        let limit = call
            .arguments
            .keywords
            .first()
            .map(|k| integer(&k.value))
            .transpose()?
            .unwrap_or(mode.parent_budget() as i64);
        let bound = u32::try_from(limit)
            .ok()
            .and_then(NonZeroU32::new)
            .filter(|n| n.get() <= mode.parent_budget())
            .ok_or(PythonLoweringInvariantError::ScopeParentBoundExceeded)?;
        let source = match source_override {
            Some(source) => source.to_owned(),
            None => self.expr(&attr.value, None)?,
        };
        let callback = self.callback(&call.arguments.args[0])?;
        self.scoped_callback_body(e, &source, &callback, bound, mode)
    }

    pub(super) fn scoped_body(
        &mut self,
        e: &PyExpr,
        source: &str,
        lambda: &ruff_python_ast::ExprLambda,
        bound: NonZeroU32,
        mode: ScopeMode,
    ) -> Result<CorrelatedBody, PythonLoweringError> {
        let callback = self.callback(&PyExpr::Lambda(lambda.clone()))?;
        self.scoped_callback_body(e, source, &callback, bound, mode)
    }

    pub(super) fn scoped_callback_body(
        &mut self,
        e: &PyExpr,
        source: &str,
        callback: &super::callbacks::Callback,
        bound: NonZeroU32,
        mode: ScopeMode,
    ) -> Result<CorrelatedBody, PythonLoweringError> {
        if self.frame.depth >= 16 {
            return Err(at(
                e,
                PythonSourceError::CallbackScopeDepth {
                    max: 16,
                    actual: self.frame.depth,
                },
            ));
        }
        let lambda = &callback.lambda;
        if callback
            .identity
            .as_ref()
            .is_some_and(|id| self.active_callbacks.contains(id))
        {
            return Err(at(
                e,
                PythonSourceError::RecursiveCallback {
                    callback: callback.identity.clone(),
                },
            ));
        }
        let source = source.to_owned();
        let flatten = mode == ScopeMode::Rows;
        let contract = super::super::binding_contract(&self.state, &source)
            .ok_or(PythonLoweringInvariantError::MapSourceContractMissing)?;
        let owner = super::super::schema_validate::resolve_qualified_entity_for_dag_source(
            &self.state,
            &[],
            source.clone(),
        )
        .or_else(|| {
            (!contract.supports_method_invoke()).then(|| QualifiedEntityKey {
                entry_id: self.es.entry_id.clone(),
                entity: "__value".into(),
            })
        })
        .ok_or(PythonLoweringInvariantError::MapCatalogProvenanceMissing)?;
        if let Some(annotation) = lambda
            .parameters
            .as_ref()
            .and_then(|p| p.posonlyargs.iter().chain(&p.args).next())
            .and_then(|p| p.parameter.annotation.as_deref())
        {
            let schema = super::text::inferred_schema(self.es, &self.state, &source, 0)?;
            crate::python_compute::check_row_parameter(
                self.es,
                annotation,
                &schema.row_contract()?,
                &self.imports.source,
            )?;
        }
        if let Some(parameters) = &lambda.parameters {
            for parameter in parameters
                .posonlyargs
                .iter()
                .chain(&parameters.args)
                .chain(&parameters.kwonlyargs)
                .map(|p| &p.parameter)
                .chain(parameters.vararg.iter().map(|p| p.as_ref()))
                .chain(parameters.kwarg.iter().map(|p| p.as_ref()))
            {
                let label = parameter.name.as_str();
                if label == "self"
                    || label.starts_with("__")
                    || self
                        .state
                        .sym_map_for(self.es)
                        .resolve_session_entity(label)
                        .is_ok()
                {
                    return Err(at(
                        e,
                        PythonSourceError::CallbackParameterShadowsHost {
                            name: label.to_owned(),
                        },
                    ));
                }
            }
        }
        let row = super::projection::projection_parameter(lambda)?;
        if row == "self"
            || row.starts_with("__")
            || self
                .state
                .sym_map_for(self.es)
                .resolve_session_entity(row)
                .is_ok()
        {
            return Err(at(
                e,
                PythonSourceError::MapParameterShadowsReserved {
                    name: row.to_owned(),
                },
            ));
        }
        let mut scope_names = callback
            .closure
            .clone()
            .unwrap_or_else(|| self.frame.names.clone());
        if callback.closure.is_none() {
            if let Some(parameters) = &lambda.parameters {
                for parameter in parameters
                    .posonlyargs
                    .iter()
                    .chain(&parameters.args)
                    .chain(&parameters.kwonlyargs)
                {
                    if let Some(default) = &parameter.default {
                        let binding = self.expr(default, None)?;
                        scope_names.insert(parameter.parameter.name.to_string(), binding);
                    }
                }
            }
        }
        if let Some(parameters) = &lambda.parameters {
            for parameter in parameters
                .posonlyargs
                .iter()
                .chain(&parameters.args)
                .chain(&parameters.kwonlyargs)
            {
                if parameter.parameter.name.as_str() == row {
                    continue;
                }
                if let Some(annotation) = &parameter.parameter.annotation {
                    let binding = scope_names
                        .get(parameter.parameter.name.as_str())
                        .ok_or(PythonLoweringInvariantError::CallbackDefaultDependencyMissing)?;
                    let schema = super::text::inferred_schema(self.es, &self.state, binding, 0)?;
                    let actual = if super::super::binding_contract(&self.state, binding)
                        .is_some_and(|c| c.value_kind == BindingValueKind::ScalarCell)
                    {
                        schema
                            .fields
                            .first()
                            .and_then(|f| f.value_type.clone())
                            .ok_or(
                                PythonLoweringInvariantError::CallbackDefaultValueContractMissing,
                            )?
                    } else {
                        schema.row_contract()?
                    };
                    let input = super::text::inferred_schema(self.es, &self.state, &source, 0)?
                        .row_contract()?;
                    crate::python_compute::check_callback_return(
                        self.es,
                        annotation,
                        &input,
                        &actual,
                        &self.imports.source,
                    )?;
                }
            }
        }
        let local = format!("__scope{}", self.frame.depth);
        scope_names.insert(row.to_owned(), local.clone());
        let row = local.as_str();
        let row_node = super::super::row_suffix_to_compute(
            self.es,
            &self.state,
            &[],
            &RowSuffix::Limit { count: 1 },
            &source,
            row,
            "",
        )?;
        let state = super::super::row_suffix::compile_state_with_nodes(&self.state, &[row_node]);
        let initial = state.nodes.len();
        let mut callbacks = callback
            .lexical_callbacks
            .as_deref()
            .cloned()
            .unwrap_or_else(|| self.callbacks.clone());
        // Self remains visible solely so the active-identity check rejects recursion.
        // Other callable names come from the definition site, never the caller.
        if let Some(binding) = &callback.binding {
            callbacks.insert(binding.clone(), callback.clone());
        }
        let frame = self.frame.nested(row.to_owned(), scope_names);
        let mut scoped = Lower {
            imports: self.imports,
            es: self.es,
            methods: self.methods,
            helpers: self.helpers,
            used_methods: self.used_methods.clone(),
            callbacks,
            active_callbacks: self.active_callbacks.clone(),
            return_check: if let Some(annotation) = &callback.returns {
                Some((
                    annotation.clone(),
                    super::text::inferred_schema(self.es, &self.state, &source, 0)?
                        .row_contract()?,
                ))
            } else if callback.identity.is_none() && callback.flow.is_some() {
                self.return_check.clone()
            } else {
                None
            },
            program_source: self.program_source,
            state,
            serial: self.serial,
            value_depth: 0,
            static_expansions: self.static_expansions,
            spans: BTreeMap::new(),
            frame,
            static_sequences: self.static_sequences.clone(),
        };
        if let Some(identity) = &callback.identity {
            scoped.active_callbacks.push(identity.clone());
        }
        // Bindings come from the upstream semantic index. Structural branches
        // reuse this namespace; they do not re-interpret assignment syntax.
        for label in callback.locals.iter().cloned() {
            // Structural branches continue the same lexical function scope.
            // Named functions allocate fresh locals even when shadowing captures.
            if callback.identity.is_none()
                && callback.closure.is_some()
                && scoped.frame.names.contains_key(&label)
            {
                continue;
            }
            let local = scoped.fresh();
            scoped.frame.names.insert(label, local);
        }
        let output = scoped.callback_sequence(&callback.flow(), row, mode)?;
        let (output, output_contract) = if flatten {
            let contract = super::super::binding_contract(&scoped.state, &output)
                .ok_or(PythonLoweringInvariantError::ScopedOutputContractMissing)?;
            if contract.value_kind == BindingValueKind::ScalarCell {
                return Err(at(
                    lambda.body.as_ref(),
                    PythonSourceError::FlatMapScalarResult,
                ));
            }
            let owner = super::super::schema_validate::resolve_qualified_entity_for_dag_source(
                &scoped.state,
                &[],
                output.clone(),
            )
            .or_else(|| {
                (!contract.supports_method_invoke()).then(|| QualifiedEntityKey {
                    entry_id: self.es.entry_id.clone(),
                    entity: "__value".into(),
                })
            })
            .ok_or(PythonLoweringInvariantError::ScopedResultProvenanceMissing)?;
            let node = scoped
                .state
                .get(&output)
                .ok_or(PythonLoweringInvariantError::ScopedResultNodeMissing)?;
            let acknowledgement = super::super::plan_serialize::lower_plan_node(node).result_shape
                == ResultShape::SideEffectAck;
            let schema = if acknowledgement {
                SyntheticResultSchema {
                    optional_fields: Default::default(),
                    entity: None,
                    fields: vec![],
                }
            } else {
                super::text::inferred_schema(self.es, &scoped.state, &output, 0)?
            };
            (
                output,
                ScopedOutput::Rows {
                    entity: PlanQualifiedEntityKey {
                        entry_id: owner.entry_id,
                        entity: owner.entity,
                    },
                    schema,
                    entity_authority: contract.supports_method_invoke(),
                    acknowledgement,
                },
            )
        } else {
            (
                output,
                match mode {
                    ScopeMode::Filter => ScopedOutput::Filter,
                    ScopeMode::Quantify(all) => ScopedOutput::Quantify { all },
                    _ => ScopedOutput::Record,
                },
            )
        };
        let nodes = scoped
            .state
            .nodes
            .iter()
            .map(|n| super::super::plan_serialize::lower_plan_node(n))
            .collect::<Vec<_>>();
        let mut plan = crate::plasm_plan::Plan::from_nodes(
            None,
            nodes,
            crate::plasm_plan::PlanReturn::Node { node: output },
            BTreeMap::new(),
        );
        super::super::plan_serialize::stamp_plan_uses_result_qualified_entities(&mut plan)?;
        let validated = crate::plasm_plan::validate_plan_artifact(&plan)?;
        let mut body = crate::plasm_comp_wire::plasm_comp_from_validated(&validated).comp;
        let locals: BTreeSet<_> = scoped.state.nodes[initial..]
            .iter()
            .map(|n| StepId::new(&n.id))
            .collect::<Result<_, _>>()?;
        // Port dependencies come from operands, not incidental order edges in the outer plan.
        let mut external = BTreeSet::new();
        for node in validated
            .nodes()
            .iter()
            .filter(|n| locals.contains(&StepId(n.id().to_string())))
        {
            external.extend(
                node.uses_result()
                    .iter()
                    .map(|u| StepId(u.node.clone()))
                    .filter(|id| !locals.contains(id)),
            );
        }
        // Returning an enclosing binding is itself a use, even when the scope
        // has no local operations. Seal it through the same typed capture port.
        if let PlasmReturn::Step { step } = &body.return_ {
            if !locals.contains(step) {
                external.insert(step.clone());
            }
        }
        external.insert(StepId::new(row)?);
        body.steps
            .retain(|id, _| locals.contains(&StepId(id.clone())));
        body.bind.topo.retain(|id| locals.contains(id));
        body.bind.deps.retain(|id, _| locals.contains(id));
        for deps in body.bind.deps.values_mut() {
            deps.retain(|id| locals.contains(id) || external.contains(id));
        }
        body.bind.holes.retain(|id, _| locals.contains(id));
        body.bind.primary.retain(|id, from| {
            locals.contains(id) && (locals.contains(from) || external.contains(from))
        });
        body.metadata.clear();
        body.metadata.insert(
            "python_source_spans".into(),
            serde_json::json!(scoped.spans),
        );
        let mut captures = Vec::new();
        for id in external.iter().filter(|id| id.as_str() != row) {
            let contract = super::super::binding_contract(&scoped.state, id.as_str())
                .ok_or(PythonLoweringInvariantError::CaptureContractMissing)?;
            let owner = super::super::schema_validate::resolve_qualified_entity_for_dag_source(
                &scoped.state,
                &[],
                id.to_string(),
            )
            .or_else(|| {
                (!contract.supports_method_invoke()).then(|| QualifiedEntityKey {
                    entry_id: self.es.entry_id.clone(),
                    entity: "__value".into(),
                })
            })
            .ok_or(PythonLoweringInvariantError::CapturedValueProvenanceMissing)?;
            let schema = super::text::inferred_schema(self.es, &scoped.state, id.as_str(), 0)?;
            let value_contract = if contract.value_kind == BindingValueKind::ScalarCell {
                Some(
                    schema
                        .fields
                        .first()
                        .and_then(|field| field.value_type.clone())
                        .ok_or(PythonLoweringInvariantError::CaptureScalarContractMissing)?,
                )
            } else {
                None
            };
            captures.push(ScopedCapture {
                source: id.clone(),
                local: id.clone(),
                entity: PlanQualifiedEntityKey {
                    entry_id: owner.entry_id,
                    entity: owner.entity,
                },
                schema,
                value_contract,
                singleton: contract.row_cardinality.permits_scalar_field_extract(),
                entity_authority: contract.supports_method_invoke(),
            });
        }
        self.serial = scoped.serial;
        self.static_expansions = scoped.static_expansions;
        self.used_methods = scoped.used_methods;
        let result = CorrelatedBody {
            output: output_contract,
            parent: ParentCapture {
                source: StepId::new(&source)?,
                local: StepId::new(row)?,
                entity: PlanQualifiedEntityKey {
                    entry_id: owner.entry_id,
                    entity: owner.entity,
                },
            },
            parent_entity_authority: contract.supports_method_invoke(),
            parent_schema: Some(super::text::inferred_schema(
                self.es,
                &self.state,
                &source,
                0,
            )?),
            captures,
            max_parents: bound,
            body,
        };
        result.execution_layers()?;
        Ok(result)
    }

    /// Assemble values using ordinary nodes, never evaluating outer Python.
    pub(super) fn scoped_value(
        &mut self,
        e: &PyExpr,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, PythonLoweringError> {
        if self.value_depth >= 64 {
            return Err(at(
                e,
                PythonSourceError::ValueExpressionDepth {
                    max: 64,
                    actual: self.value_depth,
                },
            ));
        }
        self.value_depth += 1;
        let result = self.scoped_value_inner(e, inputs);
        self.value_depth -= 1;
        let value = result?;
        if self.frame.quantifiers.is_empty() {
            if let Some(evidence) = self
                .reference(e)
                .and_then(|reference| self.frame.facts.get(&reference))
                .cloned()
            {
                let original = self.value_type(&value, inputs)?;
                let contract = original.refined_by(&evidence)?;
                if contract != original {
                    return Ok(PlasmDataValue::Expression {
                        expression: plasm_core::value_expression::ValueOperation::Refine {
                            value: Box::new(value),
                            contract,
                        },
                    });
                }
            }
        }
        Ok(value)
    }
    fn scoped_value_inner(
        &mut self,
        e: &PyExpr,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, PythonLoweringError> {
        if let PyExpr::ListComp(comprehension) = e {
            return self.static_list_comprehension(e, comprehension, inputs);
        }
        if super::quantifiers::expression_root(e)
            .is_some_and(|name| self.frame.quantifiers.contains_key(name))
        {
            return match e {
                PyExpr::Name(name) => Ok(PlasmDataValue::BindingSymbol {
                    binding: self.frame.quantifiers[name.id.as_str()].clone(),
                    path: vec![],
                }),
                PyExpr::Attribute(attr) => Ok(PlasmDataValue::Expression {
                    expression: plasm_core::value_expression::ValueOperation::Field {
                        value: Box::new(self.scoped_value(&attr.value, inputs)?),
                        name: attr.attr.to_string(),
                    },
                }),
                _ => unreachable!(),
            };
        }
        if let Some(value) = self.value_operation(e, inputs)? {
            return Ok(value);
        }
        if let Some(value) = LiteralOperand::classify(e) {
            return self.scoped_literal(e, value, inputs);
        }
        Ok(match e {
            PyExpr::Attribute(_) => {
                if let PyExpr::Attribute(attr) = e {
                    if let Some(owner) = self.expression_owner(&attr.value) {
                        if super::super::relation::resolve_relation_wire_on_entity(
                            self.es,
                            self.state.cross_cache,
                            &owner,
                            attr.attr.as_str(),
                            None,
                        )
                        .is_some()
                        {
                            return self.scoped_node_value(e, inputs);
                        }
                    }
                }
                let (node, path) = match self.field_input(e)? {
                    plasm_core::PlasmInputRef::NodeInput { node, path } => (node, path),
                    plasm_core::PlasmInputRef::RowBinding { binding, path } => {
                        return Ok(PlasmDataValue::BindingSymbol { binding, path });
                    }
                };
                inputs.insert(
                    node.clone(),
                    PlanDataInput {
                        node: node.clone(),
                        alias: node.clone(),
                        cardinality: InputCardinality::Singleton,
                    },
                );
                PlasmDataValue::NodeSymbol {
                    node: node.clone(),
                    alias: node,
                    path,
                }
            }
            // A bound rowset is a value dependency just like a rowset-producing
            // expression. Resolve it through the scope ports, never as a literal.
            PyExpr::Name(_) | PyExpr::Call(_) | PyExpr::FString(_) => {
                return self.scoped_node_value(e, inputs);
            }
            _ => PlasmDataValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::new(literal(e)?)
                    .map_err(|_| PythonLoweringInvariantError::InvalidResolvedLiteral)?,
            },
        })
    }

    fn scoped_node_value(
        &mut self,
        e: &PyExpr,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, PythonLoweringError> {
        let node = self.expr(e, None)?;
        let is_compute = matches!(
            &self
                .state
                .get(&node)
                .ok_or(PythonLoweringInvariantError::ScopedResultNodeMissing)?
                .source,
            super::super::types::DagNodeSource::Compute {
                op: ComputeOp::Python { .. },
                ..
            }
        );
        let scalar = super::super::binding_contract(&self.state, &node)
            .is_some_and(|contract| contract.value_kind == BindingValueKind::ScalarCell);
        let record = matches!(
            &self
                .state
                .get(&node)
                .ok_or(PythonLoweringInvariantError::ValueResultNodeMissing)?
                .source,
            super::super::types::DagNodeSource::Derive {
                value_type: Some(_),
                ..
            }
        ) && self.state.get(&node).is_some_and(|node| node.singleton);
        let acknowledgement = matches!(
            &self
                .state
                .get(&node)
                .ok_or(PythonLoweringInvariantError::EffectResultNodeMissing)?
                .source,
            super::super::types::DagNodeSource::Surface {
                result_shape: ResultShape::SideEffectAck,
                ..
            }
        );
        inputs.insert(
            node.clone(),
            PlanDataInput {
                node: node.clone(),
                alias: node.clone(),
                cardinality: if acknowledgement {
                    InputCardinality::Acknowledgement
                } else if self.frame.ports.contains(&node)
                    || self.frame.row.as_deref() == Some(node.as_str())
                    || record
                    || ((is_compute || scalar)
                        && super::super::binding_contract(&self.state, &node)
                            .is_some_and(|c| c.row_cardinality.permits_scalar_field_extract()))
                {
                    InputCardinality::Singleton
                } else {
                    InputCardinality::Collection
                },
            },
        );
        Ok(PlasmDataValue::NodeSymbol {
            node: node.clone(),
            alias: node,
            path: vec![],
        })
    }
    fn scoped_literal(
        &mut self,
        e: &PyExpr,
        value: LiteralOperand<'_>,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, PythonLoweringError> {
        Ok(match value {
            LiteralOperand::Record(dict) => {
                let mut fields = BTreeMap::new();
                for item in &dict.items {
                    let key_expression = item
                        .key
                        .as_ref()
                        .ok_or(PythonLoweringInvariantError::DictionaryUnpackingNotAdmitted)?;
                    // A materialized record needs a closed field contract.
                    let key = string(key_expression).map_err(|error| {
                        at(
                            key_expression,
                            error.with_context(crate::program_rejection::PythonLoweringContext::DictionaryKeyRequiresString),
                        )
                    })?;
                    let value = self.scoped_value(&item.value, inputs)?;
                    if fields.insert(key.clone(), value).is_some() {
                        return Err(at(
                            e,
                            PythonSourceError::DuplicateOutputField { field: key },
                        ));
                    }
                }
                PlasmDataValue::Object { fields }
            }
            LiteralOperand::Array(list) => PlasmDataValue::Array {
                items: list
                    .elts
                    .iter()
                    .map(|e| self.scoped_value(e, inputs))
                    .collect::<Result<_, _>>()?,
            },
            scalar @ (LiteralOperand::Text(_)
            | LiteralOperand::Number(_)
            | LiteralOperand::Signed(_)
            | LiteralOperand::Boolean(_)
            | LiteralOperand::Null(())) => PlasmDataValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::new(scalar.scalar(e)?)
                    .map_err(|_| PythonLoweringInvariantError::InvalidResolvedLiteral)?,
            },
        })
    }
}
