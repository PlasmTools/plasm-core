//! Callback control flow elaborates to reviewed, mutually exclusive row scopes.
use super::body::ScopeMode;
use super::callbacks::Callback;
use super::*;
use std::num::NonZeroU32;

impl Lower<'_> {
    pub(super) fn callback_sequence(
        &mut self,
        statements: &[Stmt],
        row: &str,
        mode: ScopeMode,
    ) -> Result<String, String> {
        for (index, statement) in statements.iter().enumerate() {
            match statement {
                Stmt::If(branch) => {
                    return self.callback_choice(branch, &statements[index + 1..], row, mode)
                }
                Stmt::Return(ret) => {
                    let none = *ruff_python_parser::parse_expression("None")
                        .map_err(|e| e.to_string())?
                        .into_syntax()
                        .body;
                    return self.callback_return(ret.value.as_deref().unwrap_or(&none), row, mode);
                }
                _ => {
                    self.statement(statement)?;
                }
            }
        }
        Err("callback path has no return".into())
    }
    fn callback_return(
        &mut self,
        expression: &PyExpr,
        row: &str,
        mode: ScopeMode,
    ) -> Result<String, String> {
        if mode == ScopeMode::Rows {
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
            let mut truth = *ruff_python_parser::parse_expression("bool(None)")
                .map_err(|e| e.to_string())?
                .into_syntax()
                .body;
            let PyExpr::Call(call) = &mut truth else {
                unreachable!()
            };
            call.arguments.args = vec![value].into();
            value = truth;
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
        branch: &ruff_python_ast::StmtIf,
        tail: &[Stmt],
        row: &str,
        mode: ScopeMode,
    ) -> Result<String, String> {
        // A condition is materialized once before either gate; branch bodies only
        // consume the Boolean. Monty owns its truth conversion and refinements.
        let condition = self.callback_return(&branch.test, row, ScopeMode::PredicateValue)?;
        let condition_schema = super::text::inferred_schema(self.es, &self.state, &condition, 0)?;
        let mut yes = branch.body.to_vec();
        yes.extend_from_slice(tail);
        let mut no = Vec::new();
        if let Some((first, rest)) = branch.elif_else_clauses.split_first() {
            if let Some(test) = &first.test {
                let mut next = branch.clone();
                next.test = Box::new(test.clone());
                next.body = first.body.clone();
                next.elif_else_clauses = rest.to_vec();
                no.push(Stmt::If(next));
            } else {
                no.extend_from_slice(&first.body);
            }
        }
        no.extend_from_slice(tail);
        let mut results = Vec::new();
        for (selected, statements) in [(true, yes), (false, no)] {
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
            let callback = Callback::branch(&parameter, statements, self.scope_names.clone())?;
            let empty = mode == ScopeMode::Rows && callback.returns_only_none();
            let child_mode = if matches!(
                mode,
                ScopeMode::Filter | ScopeMode::Quantify(_) | ScopeMode::PredicateValue
            ) {
                ScopeMode::PredicateValue
            } else {
                mode
            };
            let body = self.under_condition(&branch.test, selected, |lower| {
                lower.scoped_callback_body(
                    &branch.test,
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
                singleton: false,
                page_size: None,
                source: super::super::types::DagNodeSource::MapBody {
                    body: Box::new(body),
                    schema: schema.clone(),
                },
            })?;
            results.push((id, schema, acknowledgement, empty));
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
