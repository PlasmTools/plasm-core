//! Callback control flow elaborates to reviewed, mutually exclusive row scopes.
use super::body::ScopeMode;
use super::callbacks::Callback;
use super::*;
use std::num::NonZeroU32;

impl Lower<'_> {
    pub(super) fn callback_sequence(
        &mut self,
        flow: &monty_analysis::FunctionFlow,
        row: &str,
        mode: ScopeMode,
    ) -> Result<String, PythonLoweringError> {
        for statement in &flow.statements {
            self.statement(statement)?;
        }
        match &flow.exit {
            monty_analysis::FlowExit::Branch {
                source_expression: Some(expression),
                ..
            } if matches!(
                mode,
                ScopeMode::Value
                    | ScopeMode::Filter
                    | ScopeMode::PredicateValue
                    | ScopeMode::Quantify(_)
            ) && self.expression_placement(expression) != ExpressionPlacement::LazyHost =>
            {
                self.callback_return(expression, row, mode)
            }
            monty_analysis::FlowExit::Branch {
                test,
                selected,
                yes,
                no,
                ..
            } => self.callback_choice(test, *selected, yes, no, row, mode),
            monty_analysis::FlowExit::Return(value) => {
                let none = *ruff_python_parser::parse_expression("None")
                    .map_err(|e| e.to_string())?
                    .into_syntax()
                    .body;
                self.callback_return(value.as_ref().unwrap_or(&none), row, mode)
            }
        }
    }
    fn callback_return(
        &mut self,
        expression: &PyExpr,
        row: &str,
        mode: ScopeMode,
    ) -> Result<String, PythonLoweringError> {
        if let Some((annotation, input)) = self.return_check.take() {
            // Check the Python return before predicate truth conversion or row
            // acknowledgement adaptation. This node is pure and evaluated once.
            let raw = self.callback_return(
                expression,
                row,
                if matches!(mode, ScopeMode::Rows | ScopeMode::Record) {
                    mode
                } else {
                    ScopeMode::Value
                },
            );
            self.return_check = Some((annotation.clone(), input.clone()));
            let raw = raw?;
            let actual = if matches!(expression, PyExpr::NoneLiteral(_)) {
                plasm_core::value_contract::ValueContract {
                    shape: plasm_core::value_contract::ValueShape::Null,
                    domain: None,
                    nullable: true,
                }
            } else {
                let contract =
                    super::text::inferred_schema(self.es, &self.state, &raw, 0)?.row_contract()?;
                if matches!(mode, ScopeMode::Rows | ScopeMode::Record) {
                    contract
                } else {
                    contract.field("value")?
                }
            };
            if monty_analysis::external_names(&format!(
                "({})",
                monty::expression_source(expression)
            ))?
            .is_empty()
            {
                crate::python_compute::check_callback_closed_return(
                    self.es,
                    &annotation,
                    &input,
                    expression,
                    &self.imports.source,
                )?;
            } else if matches!(expression, PyExpr::Dict(_)) {
                crate::python_compute::check_callback_record_return(
                    self.es,
                    &annotation,
                    &input,
                    &actual,
                    &self.imports.source,
                )?;
            } else {
                crate::python_compute::check_callback_return(
                    self.es,
                    &annotation,
                    &input,
                    &actual,
                    &self.imports.source,
                )?;
            }
            if matches!(mode, ScopeMode::Rows | ScopeMode::Record | ScopeMode::Value) {
                return Ok(raw);
            }
            let value = *ruff_python_parser::parse_expression(&format!("{raw}.value"))
                .map_err(|e| e.to_string())?
                .into_syntax()
                .body;
            let check = self.return_check.take();
            let result = self.callback_return(&value, row, mode);
            self.return_check = check;
            return result;
        }
        if mode == ScopeMode::Rows {
            if let PyExpr::List(items) = expression {
                // A returned collection of effect acknowledgements is a program
                // sequence. It is not a materialized scalar array. Each child is
                // lowered in source order; the scope ledger records all writes.
                let mut effects = 0usize;
                for item in &items.elts {
                    let id = self.expr(item, None)?;
                    let node = self.state.get(&id).ok_or("missing effect sequence item")?;
                    if super::super::plan_serialize::lower_plan_node(node)?.result_shape
                        == ResultShape::SideEffectAck
                    {
                        effects += 1;
                    }
                }
                if effects == items.elts.len() && effects > 0 {
                    // Actions are already in the scope ledger. The list itself
                    // is neither a rowset nor another acknowledgement; returning
                    // it contributes no data rows.
                    let none = *ruff_python_parser::parse_expression("None")
                        .map_err(|error| error.to_string())?
                        .into_syntax()
                        .body;
                    return self.callback_return(&none, row, mode);
                }
                if effects > 0 {
                    return Err(at(
                        expression,
                        "a callback result cannot mix effects and values",
                    ));
                }
            }
            if matches!(expression, PyExpr::NoneLiteral(_)) {
                let id = self.fresh();
                let node = super::super::row_suffix_to_compute(
                    self.es,
                    &self.state,
                    &[],
                    &RowSuffix::Limit { count: 0 },
                    row,
                    &id,
                    "",
                )?;
                return self.insert(node);
            }
            return self.expr(expression, None);
        }
        let mut inputs = BTreeMap::new();
        let mut value = expression.clone();
        let predicate = matches!(
            mode,
            ScopeMode::Filter | ScopeMode::Quantify(_) | ScopeMode::PredicateValue
        );
        if predicate {
            value = super::branching::truth_test(&value)?;
        }
        let value = if predicate {
            self.pure_branch_value(&value, &mut inputs)?
        } else {
            self.scoped_value(&value, &mut inputs)?
        };
        let value = if predicate || mode == ScopeMode::Value {
            PlasmDataValue::Object {
                fields: BTreeMap::from([(
                    if predicate { "predicate" } else { "value" }.into(),
                    value,
                )]),
            }
        } else {
            value
        };
        let id = self.fresh();
        self.insert(DagNode {
            id,
            expr: String::new(),
            singleton: true,
            page_size: None,
            source: super::super::types::DagNodeSource::Derive {
                value_type: None,
                source: row.into(),
                value,
                inputs: inputs.into_values().collect(),
            },
        })
    }
    fn callback_choice(
        &mut self,
        test: &PyExpr,
        known: Option<bool>,
        yes: &monty_analysis::FunctionFlow,
        no: &monty_analysis::FunctionFlow,
        row: &str,
        mode: ScopeMode,
    ) -> Result<String, PythonLoweringError> {
        // A condition is materialized once before either gate; branch bodies only
        // consume the Boolean. Monty owns its truth conversion and refinements.
        let check = self.return_check.take();
        let condition = self.callback_return(test, row, ScopeMode::PredicateValue);
        self.return_check = check;
        let condition = condition?;
        let condition_schema = super::text::inferred_schema(self.es, &self.state, &condition, 0)?;
        let mut results = Vec::new();
        for (selected, flow) in [(true, yes), (false, no)] {
            if known.is_some_and(|known| known != selected) {
                continue;
            }
            let gate = self.fresh();
            self.insert(DagNode {
                id: gate.clone(),
                expr: String::new(),
                singleton: false,
                page_size: None,
                source: super::super::types::DagNodeSource::Compute {
                    source: condition.clone(),
                    op: ComputeOp::Filter {
                        predicates: vec![plasm_core::plasm_monad::PlanPredicate {
                            field_path: FieldPath::new(vec!["predicate".into()])?,
                            op: PlanPredicateOp::Eq,
                            value: PlasmDataValue::Literal {
                                value: plasm_core::Value::Bool(selected).try_into()?,
                            },
                        }]
                        .into(),
                    },
                    schema: condition_schema.clone(),
                    collection_alias: None,
                },
            })?;
            let parameter = self.fresh_parameter("callback_branch");
            let callback = Callback::branch(&parameter, flow.clone(), self.frame.names.clone())?;
            let empty = mode == ScopeMode::Rows && flow.returns_only_none();
            let child_mode = if matches!(
                mode,
                ScopeMode::Filter | ScopeMode::Quantify(_) | ScopeMode::PredicateValue
            ) {
                ScopeMode::PredicateValue
            } else {
                mode
            };
            let body = self.under_condition(test, selected, |lower| {
                lower.scoped_callback_body(
                    test,
                    &gate,
                    &callback,
                    NonZeroU32::new(1).unwrap(),
                    child_mode,
                )
            })?;
            let schema = crate::map_body_schema::output_schema(self.es, &body)?;
            let acknowledgement = body.result_shape() == ResultShape::SideEffectAck;
            let id = self.fresh();
            self.insert(DagNode {
                id: id.clone(),
                expr: String::new(),
                // Upstream proved this successor is selected; its gate still
                // evaluates the condition, but cannot remove the parent row.
                singleton: known.is_some() && mode != ScopeMode::Rows,
                page_size: None,
                source: super::super::types::DagNodeSource::MapBody {
                    body: Box::new(body),
                    schema: schema.clone(),
                },
            })?;
            results.push((id, schema, acknowledgement, empty));
        }
        if results.len() == 1 {
            let (node, schema, _, _) = results.pop().unwrap();
            if mode == ScopeMode::Rows {
                return Ok(node);
            }
            // Materialize the proven single successor through the ordinary
            // singleton input boundary (which also checks the count at runtime).
            let value = PlasmDataValue::Object {
                fields: schema
                    .fields
                    .iter()
                    .map(|field| {
                        (
                            field.name.to_string(),
                            PlasmDataValue::NodeSymbol {
                                node: node.clone(),
                                alias: node.clone(),
                                path: vec![field.name.to_string()],
                            },
                        )
                    })
                    .collect(),
            };
            let id = self.fresh();
            return self.emit_value(
                value,
                vec![crate::plasm_plan::PlanDataInput {
                    node: node.clone(),
                    alias: node,
                    cardinality: crate::plasm_plan::InputCardinality::Singleton,
                }],
                &id,
            );
        }
        let right = results.pop().unwrap();
        let left = results.pop().unwrap();
        if mode == ScopeMode::Rows {
            if right.3 {
                return Ok(left.0);
            }
            if left.3 {
                return Ok(right.0);
            }
            if left.2 && right.2 {
                // Acknowledgements are the enclosing scope's operation ledger,
                // accumulated from every executed child, not materialized rows.
                return Ok(right.0);
            }
            if left.2 != right.2 {
                return Err("callback branches cannot mix rows and acknowledgements".into());
            }
            let id = self.fresh();
            let node = super::super::row_suffix_to_compute(
                self.es,
                &self.state,
                &[],
                &RowSuffix::Union { rhs: right.0 },
                &left.0,
                &id,
                "",
            )?;
            return self.insert(node);
        }
        let mut schema = left.1;
        if schema
            .fields
            .iter()
            .map(|f| &f.name)
            .collect::<BTreeSet<_>>()
            != right.1.fields.iter().map(|f| &f.name).collect()
        {
            return Err("callback branches must return the same record fields".into());
        }
        for field in &mut schema.fields {
            let other = right
                .1
                .fields
                .iter()
                .find(|f| f.name == field.name)
                .unwrap();
            let value = plasm_core::value_contract::ValueContract::join(
                field
                    .value_type
                    .take()
                    .ok_or("missing callback return type")?,
                other
                    .value_type
                    .clone()
                    .ok_or("missing callback return type")?,
            );
            field.value_kind = value.summary();
            field.value_type = Some(value);
        }
        schema.optional_fields.extend(right.1.optional_fields);
        let id = self.fresh();
        self.insert(DagNode {
            id,
            expr: String::new(),
            singleton: true,
            page_size: None,
            source: super::super::types::DagNodeSource::Compute {
                source: left.0,
                op: ComputeOp::MergeBranches {
                    other: OutputName::new(right.0)?,
                },
                schema,
                collection_alias: None,
            },
        })
    }
}
