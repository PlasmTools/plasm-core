//! Recursive scoped lowering. Body expressions use the ordinary DAG compiler.
use super::literal_operands::LiteralOperand;
use super::*;
use crate::plasm_plan::{InputCardinality, PlanDataInput};
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

impl Lower<'_> {
    pub(super) fn map(&mut self, e: &PyExpr) -> Result<CorrelatedBody, String> {
        self.scope(e, false, None)
    }

    pub(super) fn scope(
        &mut self,
        e: &PyExpr,
        flatten: bool,
        source_override: Option<&str>,
    ) -> Result<CorrelatedBody, String> {
        if self.scope_depth >= 16 {
            return Err(at(e, "scoped composition exceeds 16 map levels"));
        }
        let PyExpr::Call(call) = e else {
            return Err(at(e, "expected map"));
        };
        let PyExpr::Attribute(attr) = &*call.func else {
            return Err(at(e, "expected map receiver"));
        };
        if call.arguments.args.len() != 1
            || call.arguments.keywords.len() > 1
            || (!flatten && call.arguments.keywords.is_empty())
            || call
                .arguments
                .keywords
                .first()
                .is_some_and(|k| k.arg.as_ref().map(|s| s.as_str()) != Some("max_parents"))
        {
            return Err(at(e, "scope requires one callback and optional max_parents; map requires an explicit bound"));
        }
        let limit = call
            .arguments
            .keywords
            .first()
            .map(|k| integer(&k.value))
            .transpose()?
            .unwrap_or(if flatten { 65_536 } else { 256 });
        let bound = u32::try_from(limit)
            .ok()
            .and_then(NonZeroU32::new)
            .filter(|n| n.get() <= if flatten { 65_536 } else { 256 })
            .ok_or("scope parent bound exceeds execution budget")?;
        let source = match source_override {
            Some(source) => source.to_owned(),
            None => self.expr(&attr.value, None)?,
        };
        let callback = self.callback(&call.arguments.args[0])?;
        self.scoped_callback_body(
            e,
            &source,
            &callback,
            bound,
            if flatten {
                ScopeMode::Rows
            } else {
                ScopeMode::Record
            },
        )
    }

    pub(super) fn scoped_body(
        &mut self,
        e: &PyExpr,
        source: &str,
        lambda: &ruff_python_ast::ExprLambda,
        bound: NonZeroU32,
        mode: ScopeMode,
    ) -> Result<CorrelatedBody, String> {
        let callback = super::callbacks::Callback {
            lambda: lambda.clone(),
            prelude: vec![],
            closure: None,
            identity: None,
            binding: None,
            lexical_callbacks: None,
        };
        self.scoped_callback_body(e, source, &callback, bound, mode)
    }

    pub(super) fn scoped_callback_body(
        &mut self,
        e: &PyExpr,
        source: &str,
        callback: &super::callbacks::Callback,
        bound: NonZeroU32,
        mode: ScopeMode,
    ) -> Result<CorrelatedBody, String> {
        if self.scope_depth >= 16 {
            return Err(at(e, "scoped composition exceeds 16 levels"));
        }
        let lambda = &callback.lambda;
        if callback
            .identity
            .as_ref()
            .is_some_and(|id| self.active_callbacks.contains(id))
        {
            return Err(at(e, "recursive callbacks are not a bounded DAG"));
        }
        let source = source.to_owned();
        let flatten = mode == ScopeMode::Rows;
        let contract = super::super::binding_contract(&self.state, &source)
            .ok_or("map source contract missing")?;
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
        .ok_or("map rows require typed catalog provenance")?;
        let row = super::projection::projection_parameter(lambda)?;
        if row == "self"
            || row.starts_with("__")
            || self
                .state
                .sym_map_for(self.es)
                .resolve_session_entity(row)
                .is_ok()
        {
            return Err(at(e, "map parameter must not shadow a reserved binding"));
        }
        let mut scope_names = callback
            .closure
            .clone()
            .unwrap_or_else(|| self.scope_names.clone());
        let local = format!("__scope{}", self.scope_depth);
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
        let mut scoped = Lower {
            imports: self.imports,
            es: self.es,
            methods: self.methods,
            callbacks,
            active_callbacks: self.active_callbacks.clone(),
            program_source: self.program_source,
            state,
            serial: self.serial,
            row_scope: None,
            quantifier_names: BTreeMap::new(),
            branch_types: self.branch_types.clone(),
            value_depth: 0,
            spans: BTreeMap::new(),
            scope_depth: self.scope_depth + 1,
            scope_row: Some(row.into()),
            scope_names,
        };
        if let Some(identity) = &callback.identity {
            scoped.active_callbacks.push(identity.clone());
        }
        // Allocate the lexical local namespace before elaborating any RHS, so a
        // read-before-binding cannot accidentally resolve an enclosing variable.
        fn collect_locals(statements: &[Stmt], locals: &mut BTreeSet<String>) {
            for statement in statements {
                match statement {
                    Stmt::Assign(assign) => {
                        for target in &assign.targets {
                            if let Some(label) = name(target) {
                                locals.insert(label.to_owned());
                            }
                        }
                    }
                    Stmt::If(branch) => {
                        collect_locals(&branch.body, locals);
                        for clause in &branch.elif_else_clauses {
                            collect_locals(&clause.body, locals);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut locals = BTreeSet::new();
        collect_locals(&callback.prelude, &mut locals);
        for label in locals {
            if label == super::projection::projection_parameter(lambda)? {
                return Err(at(e, "rebinding is not admitted"));
            }
            // Structural branches continue the same lexical function scope.
            // Named functions allocate fresh locals even when shadowing captures.
            if callback.identity.is_none()
                && callback.closure.is_some()
                && scoped.scope_names.contains_key(&label)
            {
                continue;
            }
            let local = scoped.fresh();
            scoped.scope_names.insert(label, local);
        }
        let output = scoped.callback_sequence(&callback.statements(), row, mode)?;
        let (output, output_contract) = if flatten {
            let contract = super::super::binding_contract(&scoped.state, &output)
                .ok_or("missing scoped output contract")?;
            if contract.value_kind == BindingValueKind::ScalarCell {
                return Err(at(
                    lambda.body.as_ref(),
                    "flat_map requires rows or effects, not a scalar value",
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
            .ok_or("scoped result has no catalog provenance")?;
            let node = scoped.state.get(&output).ok_or("missing scope result")?;
            let acknowledgement = super::super::plan_serialize::lower_plan_node(node)?.result_shape
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
            .collect::<Result<Vec<_>, _>>()?;
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
                .ok_or("capture contract missing")?;
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
            .ok_or("captured value requires catalog provenance")?;
            let schema = super::text::inferred_schema(self.es, &scoped.state, id.as_str(), 0)?;
            let value_contract = if contract.value_kind == BindingValueKind::ScalarCell {
                Some(
                    schema
                        .fields
                        .first()
                        .and_then(|field| field.value_type.clone())
                        .ok_or("scalar capture contract missing")?,
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
    ) -> Result<PlasmDataValue, String> {
        if self.value_depth >= 64 {
            return Err(at(e, "value expression depth exceeds 64"));
        }
        self.value_depth += 1;
        let result = self.scoped_value_inner(e, inputs);
        self.value_depth -= 1;
        let value = result?;
        if self.quantifier_names.is_empty() {
            if let Some(evidence) = self
                .reference(e)
                .and_then(|reference| self.branch_types.get(&reference))
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
    ) -> Result<PlasmDataValue, String> {
        if super::quantifiers::expression_root(e)
            .is_some_and(|name| self.quantifier_names.contains_key(name))
        {
            return match e {
                PyExpr::Name(name) => Ok(PlasmDataValue::BindingSymbol {
                    binding: self.quantifier_names[name.id.as_str()].clone(),
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
                let node = self.expr(e, None)?;
                let is_compute = matches!(
                    &self.state.get(&node).ok_or("missing scoped result")?.source,
                    super::super::types::DagNodeSource::Compute {
                        op: ComputeOp::Python { .. },
                        ..
                    }
                );
                let scalar = super::super::binding_contract(&self.state, &node)
                    .is_some_and(|contract| contract.value_kind == BindingValueKind::ScalarCell);
                let record = matches!(
                    &self.state.get(&node).ok_or("missing value result")?.source,
                    super::super::types::DagNodeSource::Derive {
                        value_type: Some(_),
                        ..
                    }
                ) && self.state.get(&node).is_some_and(|node| node.singleton);
                let acknowledgement = matches!(
                    &self.state.get(&node).ok_or("missing effect result")?.source,
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
                        } else if self.scope_row.as_deref() == Some(node.as_str())
                            || record
                            || ((is_compute || scalar)
                                && super::super::binding_contract(&self.state, &node).is_some_and(
                                    |c| c.row_cardinality.permits_scalar_field_extract(),
                                ))
                        {
                            InputCardinality::Singleton
                        } else {
                            InputCardinality::Collection
                        },
                    },
                );
                PlasmDataValue::NodeSymbol {
                    node: node.clone(),
                    alias: node,
                    path: vec![],
                }
            }
            _ => PlasmDataValue::Literal {
                value: plasm_core::operand_binding::ResolvedValue::new(literal(e)?)?,
            },
        })
    }
    fn scoped_literal(
        &mut self,
        e: &PyExpr,
        value: LiteralOperand<'_>,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, String> {
        Ok(match value {
            LiteralOperand::Record(dict) => {
                let mut fields = BTreeMap::new();
                for item in &dict.items {
                    let key = string(
                        item.key
                            .as_ref()
                            .ok_or("dictionary unpacking is not admitted")?,
                    )?;
                    let value = self.scoped_value(&item.value, inputs)?;
                    if fields.insert(key, value).is_some() {
                        return Err(at(e, "duplicate output field"));
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
                value: plasm_core::operand_binding::ResolvedValue::new(scalar.scalar(e)?)?,
            },
        })
    }
}
