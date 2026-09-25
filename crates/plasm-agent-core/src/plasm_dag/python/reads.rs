use super::super::schema_validate::*;
use super::*;
use ruff_python_ast::{CmpOp, ExprCall};
impl Lower<'_> {
    pub(super) fn read(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        method: &str,
        owner: plasm_core::symbol_tuning::EntityBinding,
        id: &str,
    ) -> Result<String, String> {
        let cgs = crate::catalog_ownership::resolve_cgs_for_entry_entity(
            self.es,
            owner.entry_id.as_str(),
            owner.entity.as_str(),
        )?;
        let specific = if matches!(method, "query" | "get" | "search") {
            None
        } else {
            let symbols = self.state.sym_map_for(self.es);
            let binding = symbols
                .resolve_session_method(method)
                .map_err(|e| at(site, &e.to_string()))?;
            if binding.entry_id != owner.entry_id || binding.domain != owner.entity {
                return Err(at(site, "read method and entity ownership differ"));
            }
            Some(binding.capability.clone())
        };
        let method = specific
            .as_ref()
            .and_then(|cap| cgs.get_capability(cap.as_str()))
            .map(|cap| cap.kind.as_str())
            .unwrap_or(method);
        let mut expr = match method {
            "query" | "search" if call.arguments.args.is_empty() => {
                let mut q = plasm_core::QueryExpr::all(owner.entity.as_str());
                if let Some(capability) = &specific {
                    q.capability_name = Some(capability.clone());
                } else if method == "search" {
                    q.capability_name = Some(
                        cgs.primary_search_capability(owner.entity.as_str())
                            .ok_or("entity has no primary search capability")?
                            .name
                            .clone(),
                    );
                }
                let mut predicates = Vec::new();
                let mut keys = BTreeSet::new();
                for kw in &call.arguments.keywords {
                    let key = kw
                        .arg
                        .as_ref()
                        .ok_or_else(|| at(site, "keyword unpacking is not admitted"))?;
                    if !keys.insert(key.as_str()) {
                        return Err(at(site, "duplicate selection argument"));
                    }
                    let field = self
                        .state
                        .sym_map_for(self.es)
                        .resolve_query_filter_field(
                            plasm_core::symbol_tuning::CatalogScope::qualified(
                                owner.entry_id.as_str(),
                            ),
                            owner.entity.as_str(),
                            cgs.get_entity(owner.entity.as_str())
                                .ok_or("missing entity")?,
                            cgs,
                            key.as_str(),
                        )
                        .map_err(|e| e.to_string())?;
                    predicates.push(plasm_core::Predicate::eq(
                        field,
                        self.write_value(&kw.value)?,
                    ));
                }
                if !predicates.is_empty() {
                    q.predicate = Some(if predicates.len() == 1 {
                        predicates.remove(0)
                    } else {
                        plasm_core::Predicate::And { args: predicates }
                    });
                }
                q.catalog_entry_id = plasm_core::CatalogEntryStamp::some(owner.entry_id.clone());
                plasm_core::rowset::normalize_query_expr_to_rowset(
                    &q,
                    cgs,
                    owner.entry_id.as_str(),
                )?;
                plasm_core::Expr::Query(q)
            }
            "get" => {
                let entity = cgs
                    .get_entity(owner.entity.as_str())
                    .ok_or("missing Get entity")?;
                let reference = if specific
                    .as_ref()
                    .and_then(|name| cgs.get_capability(name.as_str()))
                    .or_else(|| cgs.primary_get_capability(owner.entity.as_str()))
                    .is_some_and(|cap| !cap.get_requires_identity_anchor(cgs))
                {
                    if !call.arguments.args.is_empty() || !call.arguments.keywords.is_empty() {
                        return Err(at(site, "this Get takes no identity arguments"));
                    }
                    plasm_core::GetExpr::pathless_nullary(owner.entity.as_str()).reference
                } else if entity.key_vars.len() > 1 {
                    if !call.arguments.args.is_empty() {
                        return Err(at(
                            site,
                            "compound Get requires exactly its named identity keys",
                        ));
                    }
                    let mut slots = BTreeMap::new();
                    for kw in &call.arguments.keywords {
                        let key = kw.arg.as_ref().ok_or_else(|| {
                            at(site, "identity keyword unpacking is not admitted")
                        })?;
                        if !entity.key_vars.iter().any(|k| k.as_str() == key.as_str()) {
                            return Err(at(site, "unknown compound identity key"));
                        }
                        if slots
                            .insert(key.to_string(), self.identity_slot(&kw.value)?)
                            .is_some()
                        {
                            return Err(at(site, "duplicate compound identity key"));
                        }
                    }
                    if slots.len() != entity.key_vars.len() {
                        return Err(at(site, "compound Get requires every identity key"));
                    }
                    plasm_core::Ref::compound_slots(owner.entity.as_str(), slots)
                } else {
                    if call.arguments.args.len() != 1 || !call.arguments.keywords.is_empty() {
                        return Err(at(site, "simple Get requires one positional identity"));
                    }
                    match self.identity_slot(&call.arguments.args[0])? {
                        plasm_core::IdentitySlot::Binding(input) => {
                            plasm_core::Ref::simple_binding(owner.entity.as_str(), input)
                        }
                        plasm_core::IdentitySlot::Lit(id) => {
                            plasm_core::Ref::new(owner.entity.as_str(), id.as_str())
                        }
                    }
                };
                let mut g = plasm_core::GetExpr::from_ref(reference);
                g.capability_name = specific.clone();
                g.catalog_entry_id = plasm_core::CatalogEntryStamp::some(owner.entry_id.clone());
                plasm_core::Expr::Get(g)
            }
            _ => {
                return Err(at(
                    site,
                    "supported catalog calls are query(selection=value) and get(identity)",
                ))
            }
        };
        plasm_core::apply_required_selection_defaults_in_expr(&mut expr, cgs, "")
            .map_err(|e| e.to_string())?;
        self.emit_catalog(id, expr)
    }
    fn identity_slot(&self, e: &PyExpr) -> Result<plasm_core::IdentitySlot, String> {
        let value = if let PyExpr::UnaryOp(unary) = e {
            if unary.op != ruff_python_ast::UnaryOp::USub {
                return Err(at(e, "unsupported identity expression"));
            }
            // Parse the signed spelling together so i64::MIN remains exact.
            let PyExpr::NumberLiteral(number) = &*unary.operand else {
                return Err(at(e, "identity negation requires an integer literal"));
            };
            let ruff_python_ast::Number::Int(number) = &number.value else {
                return Err(at(e, "IEEE float is not an identity literal"));
            };
            plasm_core::Value::Integer(
                format!("-{number}")
                    .parse()
                    .map_err(|_| at(e, "integer identity out of range"))?,
            )
        } else {
            self.write_value(e)?
        };
        match value {
            plasm_core::Value::PlasmInputRef(input) => Ok(plasm_core::IdentitySlot::binding(input)),
            plasm_core::Value::String(id) => Ok(plasm_core::IdentitySlot::lit(id)),
            plasm_core::Value::Bool(id) => Ok(plasm_core::IdentitySlot::lit(id.to_string())),
            plasm_core::Value::Integer(id) => Ok(plasm_core::IdentitySlot::lit(id.to_string())),
            _ => Err(at(
                e,
                "identity requires a string, exact integer, boolean or typed binding",
            )),
        }
    }
    pub(super) fn filter(
        &mut self,
        site: &PyExpr,
        source: &str,
        id: &str,
        e: &PyExpr,
    ) -> Result<String, String> {
        let predicates = self.boolean_predicates(site, source, e)?;
        let node = DagNode {
            id: id.into(),
            expr: String::new(),
            singleton: false,
            page_size: None,
            source: super::super::types::DagNodeSource::Compute {
                source: source.into(),
                op: ComputeOp::Filter { predicates },
                schema: compute_passthrough_or_fallback_schema(
                    self.es,
                    &self.state,
                    &[],
                    source,
                    "PythonFilter",
                ),
                collection_alias: None,
            },
        };
        self.insert(node)
    }
    fn boolean_predicates(
        &mut self,
        site: &PyExpr,
        source: &str,
        e: &PyExpr,
    ) -> Result<plasm_core::BooleanExpr<crate::plasm_plan::PlanPredicate>, String> {
        let PyExpr::Lambda(lambda) = e else {
            return Err(at(e, "where requires a lambda"));
        };
        self.boolean_body(site, source, lambda, &lambda.body)
    }
    fn boolean_body(
        &mut self,
        site: &PyExpr,
        source: &str,
        lambda: &ruff_python_ast::ExprLambda,
        body: &PyExpr,
    ) -> Result<plasm_core::BooleanExpr<crate::plasm_plan::PlanPredicate>, String> {
        use plasm_core::BooleanExpr;
        match body {
            PyExpr::BoolOp(op) => {
                let args = op
                    .values
                    .iter()
                    .map(|value| self.boolean_body(site, source, lambda, value))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(match op.op {
                    ruff_python_ast::BoolOp::And => BooleanExpr::And(args),
                    ruff_python_ast::BoolOp::Or => BooleanExpr::Or(args),
                })
            }
            PyExpr::UnaryOp(op) if op.op == ruff_python_ast::UnaryOp::Not => Ok(BooleanExpr::Not(
                Box::new(self.boolean_body(site, source, lambda, &op.operand)?),
            )),
            _ => {
                let mut atom = lambda.clone();
                atom.body = Box::new(body.clone());
                Ok(self
                    .comparison_predicates(site, source, &PyExpr::Lambda(atom), false)?
                    .into())
            }
        }
    }
    pub(super) fn comparison_predicates(
        &mut self,
        site: &PyExpr,
        source: &str,
        e: &PyExpr,
        allow_binding: bool,
    ) -> Result<Vec<crate::plasm_plan::PlanPredicate>, String> {
        let PyExpr::Lambda(lambda) = e else {
            return Err(at(e, "where requires a lambda"));
        };
        let p = lambda
            .parameters
            .as_ref()
            .ok_or("where requires one row parameter")?;
        if p.args.len() != 1
            || !p.posonlyargs.is_empty()
            || !p.kwonlyargs.is_empty()
            || p.vararg.is_some()
            || p.kwarg.is_some()
            || p.args[0].default.is_some()
        {
            return Err(at(e, "where requires one row parameter"));
        }
        let PyExpr::Compare(c) = &*lambda.body else {
            return Err(at(e, "where admits one scalar comparison"));
        };
        if c.ops.len() != 1 || c.comparators().len() != 1 {
            return Err(at(e, "chained comparisons are not admitted"));
        }
        let contains = c.ops[0] == CmpOp::In
            && matches!(&c.operands[0], PyExpr::StringLiteral(_))
            && matches!(&c.comparators()[0], PyExpr::Attribute(_));
        let left = if contains {
            &c.comparators()[0]
        } else {
            &c.operands[0]
        };
        let right = if contains {
            &c.operands[0]
        } else {
            &c.comparators()[0]
        };
        let literal_membership = !contains
            && matches!(c.ops[0], CmpOp::In | CmpOp::NotIn)
            && matches!(right, PyExpr::List(_) | PyExpr::Tuple(_));
        let PyExpr::Attribute(field) = left else {
            return Err(at(e, "comparison requires a row field"));
        };
        if name(&field.value) != Some(p.args[0].parameter.name.as_str()) {
            return Err(at(e, "comparison must reference its row"));
        }
        let qe = resolve_qualified_entity_for_dag_source(&self.state, &[], source.to_owned())
            .ok_or("where requires entity rows")?;
        let row_schema = super::super::schema_validate::resolve_immediate_compute_schema(
            &self.state,
            &[],
            source,
        );
        let path = super::super::schema_validate::resolve_sort_field_path(
            self.es,
            None,
            Some(&qe),
            row_schema.as_ref(),
            &FieldPath::from_dotted(field.attr.as_str())?,
        )?;
        validate_compute_paths_for_dag_source(
            self.es,
            &self.state,
            &[],
            source,
            std::slice::from_ref(&path),
            "where",
        )?;
        if !contains && !literal_membership && matches!(c.ops[0], CmpOp::In | CmpOp::NotIn) {
            if allow_binding {
                return Err(at(
                    site,
                    "iteration stop predicates require scalar comparisons",
                ));
            }
            let rhs_expr = &c.comparators()[0];
            super::membership::validate_closed_rhs(rhs_expr, p.args[0].parameter.name.as_str())?;
            let rhs = self.expr(rhs_expr, None)?;
            if super::super::binding_contract(&self.state, &rhs)
                .is_some_and(|contract| contract.is_scalar_cell())
            {
                return Err(at(site, "membership requires a rowset, not a scalar cell"));
            }
            let column_path =
                super::super::row_suffix::membership_rhs_column_path(&self.state, &[], &rhs)?;
            if column_path.len() != 1 {
                return Err(at(
                    site,
                    "membership requires an explicit one-column rowset",
                ));
            }
            return Ok(vec![crate::plasm_plan::PlanPredicate {
                field_path: path,
                op: if c.ops[0] == CmpOp::In {
                    crate::plasm_plan::PlanPredicateOp::In
                } else {
                    crate::plasm_plan::PlanPredicateOp::NotIn
                },
                value: crate::plasm_plan::PlanValue::BindingSymbol {
                    binding: rhs.to_owned(),
                    path: column_path,
                },
            }]);
        }
        let op = if contains {
            plasm_core::CompOp::Contains
        } else {
            match c.ops[0] {
                CmpOp::Eq => plasm_core::CompOp::Eq,
                CmpOp::NotEq => plasm_core::CompOp::Neq,
                CmpOp::Lt => plasm_core::CompOp::Lt,
                CmpOp::LtE => plasm_core::CompOp::Lte,
                CmpOp::Gt => plasm_core::CompOp::Gt,
                CmpOp::GtE => plasm_core::CompOp::Gte,
                CmpOp::In | CmpOp::NotIn if literal_membership => plasm_core::CompOp::In,
                _ => return Err(at(site, "unsupported comparison")),
            }
        };
        let pred = plasm_core::RowPredicate(vec![plasm_core::RowComparison {
            field: path.segments()[0].clone(),
            op,
            value: plasm_core::TypedComparisonValue::from_value(if allow_binding {
                let operand = &c.comparators()[0];
                if let Some(binding) = name(operand) {
                    if !super::super::binding_contract(&self.state, binding)
                        .is_some_and(|c| c.is_scalar_cell())
                    {
                        return Err(at(operand, "comparison operand must be a scalar cell"));
                    }
                }
                self.write_value(operand)?
            } else {
                if let PyExpr::Tuple(tuple) = right {
                    plasm_core::Value::Array(
                        tuple.elts.iter().map(literal).collect::<Result<_, _>>()?,
                    )
                } else {
                    literal(right)?
                }
            }),
        }]);
        let cgs = cgs_for_qualified_entity(self.es, &qe).ok_or("missing catalog")?;
        let mut catalog_pred = pred.clone();
        if row_schema.is_some() {
            catalog_pred.0.retain(|clause| {
                cgs.get_entity(qe.entity.as_str())
                    .is_some_and(|entity| entity.fields.contains_key(clause.field.as_str()))
            });
        }
        if literal_membership {
            let mut element_comparisons = Vec::new();
            for clause in catalog_pred.0 {
                let plasm_core::Value::Array(values) = clause.value.to_value() else {
                    return Err(at(site, "membership requires a literal list or tuple"));
                };
                element_comparisons.extend(values.into_iter().map(|value| {
                    plasm_core::RowComparison {
                        field: clause.field.clone(),
                        op: plasm_core::CompOp::Eq,
                        value: plasm_core::TypedComparisonValue::from_value(value),
                    }
                }));
            }
            catalog_pred.0 = element_comparisons;
        }
        plasm_core::type_check_row_predicate(
            &catalog_pred,
            &plasm_core::RowPredicateTypeCtx {
                qe: &plasm_core::QualifiedEntityKey::new(&qe.entry_id, &qe.entity),
                cgs: cgs.as_ref(),
                symbol_map: None,
            },
        )
        .map_err(|e| e.to_string())?;
        let mut lowered = crate::row_predicate_lower::lower_row_predicate_to_plan(
            &pred,
            self.es,
            &qe,
            None,
            &row_schema
                .map(|schema| {
                    schema
                        .fields
                        .iter()
                        .map(|field| field.name.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
        )?;
        if literal_membership && c.ops[0] == CmpOp::NotIn {
            for predicate in &mut lowered {
                predicate.op = crate::plasm_plan::PlanPredicateOp::NotIn;
            }
        }
        Ok(lowered)
    }
}
