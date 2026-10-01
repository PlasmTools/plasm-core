//! Lazy values are complementary singleton selections followed by scoped binds.
//! Both branches are reviewed; only the selected scope has a parent occurrence.
use super::literal_operands::LiteralOperand;
use super::*;
use crate::plasm_plan::{InputCardinality, PlanDataInput};
use std::num::NonZeroU32;

fn lambda(source: &str) -> Result<ruff_python_ast::ExprLambda, String> {
    let parsed = ruff_python_parser::parse_expression(source)
        .map_err(|e| format!("internal branch expression: {e}"))?;
    let PyExpr::Lambda(lambda) = *parsed.into_syntax().body else {
        return Err("internal branch template is not a lambda".into());
    };
    Ok(lambda)
}

impl Lower<'_> {
    pub(super) fn pure_branch_value(
        &mut self,
        expression: &PyExpr,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, String> {
        let start = self.state.nodes.len();
        let value = self.scoped_value(expression, inputs)?;
        for node in &self.state.nodes[start..] {
            let node = super::super::plan_serialize::lower_plan_node(node)?;
            if !matches!(
                node.effect_class,
                EffectClass::Read | EffectClass::ArtifactRead
            ) {
                return Err(at(
                    expression,
                    "lazy value expressions cannot introduce effects",
                ));
            }
        }
        Ok(value)
    }

    pub(super) fn comparison_chain(
        &mut self,
        site: &PyExpr,
        chain: &ruff_python_ast::ExprCompare,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, String> {
        let mut operands = Vec::new();
        for expression in &chain.operands[..2] {
            let mut dependencies = BTreeMap::new();
            let before = self.state.nodes.len();
            let value = self.pure_branch_value(expression, &mut dependencies)?;
            // Immutable references and scalar literals already evaluate once.
            // Keeping their identity also preserves None-test and branch facts.
            if self.state.nodes.len() == before
                && (super::refinements::Reference::of(expression).is_some()
                    || LiteralOperand::classify(expression)
                        .is_some_and(|literal| literal.scalar(expression).is_ok()))
            {
                operands.push(expression.clone());
                continue;
            }
            let id = self.fresh();
            self.emit_value(value, dependencies.into_values().collect(), &id)?;
            operands.push(
                *ruff_python_parser::parse_expression(&id)
                    .map_err(|e| e.to_string())?
                    .into_syntax()
                    .body,
            );
        }
        let mut first = chain.clone();
        first.ops = first.ops[..1].to_vec().into();
        first.operands = operands.clone().into();
        let mut tail = chain.clone();
        tail.ops = tail.ops[1..].to_vec().into();
        tail.operands = tail.operands[1..].to_vec().into();
        tail.operands[0] = operands.remove(1);
        let otherwise = PyExpr::BooleanLiteral(ruff_python_ast::ExprBooleanLiteral {
            node_index: Default::default(),
            range: site.range(),
            value: false,
        });
        self.conditional_value(
            site,
            &PyExpr::Compare(first),
            &PyExpr::Compare(tail),
            &otherwise,
            inputs,
            true,
            None,
        )
    }

    pub(super) fn conditional_value(
        &mut self,
        site: &PyExpr,
        condition: &PyExpr,
        yes: &PyExpr,
        no: &PyExpr,
        inputs: &mut BTreeMap<String, PlanDataInput>,
        predicate: bool,
        branch_condition: Option<&PyExpr>,
    ) -> Result<PlasmDataValue, String> {
        let mut condition_inputs = BTreeMap::new();
        let condition_value = self.pure_branch_value(condition, &mut condition_inputs)?;
        let condition_id = self.fresh();
        self.emit_value(
            PlasmDataValue::Object {
                fields: BTreeMap::from([("predicate".into(), condition_value)]),
            },
            condition_inputs.into_values().collect(),
            &condition_id,
        )?;
        let condition_schema =
            super::text::inferred_schema(self.es, &self.state, &condition_id, 0)?;
        if !condition_schema.fields.first().is_some_and(|field| {
            field.value_type.as_ref().is_some_and(|value| {
                !value.nullable && value.summary() == SyntheticValueKind::Boolean
            })
        }) {
            return Err(at(site, "condition requires a boolean value"));
        }
        let mut branches = Vec::new();
        let mut schemas = Vec::new();
        for (selected, value) in [(true, yes), (false, no)] {
            let parameter = self.fresh_parameter("branch");
            let source = self.fresh();
            self.insert(DagNode {
                id: source.clone(),
                expr: String::new(),
                singleton: false,
                page_size: None,
                source: super::super::types::DagNodeSource::Compute {
                    source: condition_id.clone(),
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
            let mut branch = lambda(&format!("lambda {parameter}: {{'value': None}}"))?;
            let PyExpr::Dict(record) = branch.body.as_mut() else {
                unreachable!()
            };
            record.items[0].value = value.clone();
            let body = self.under_condition(condition, selected, |lower| {
                let build = |lower: &mut Self| {
                    lower.scoped_body(
                        site,
                        &source,
                        &branch,
                        NonZeroU32::new(1).unwrap(),
                        super::body::ScopeMode::Record,
                    )
                };
                if let Some(original) = branch_condition {
                    lower.under_condition(original, selected, build)
                } else {
                    build(lower)
                }
            })?;
            if !matches!(
                body.effect_class(),
                EffectClass::Read | EffectClass::ArtifactRead
            ) {
                return Err(at(site, "lazy value expressions cannot introduce effects"));
            }
            let schema = crate::map_body_schema::output_schema(self.es, &body)?;
            if predicate
                && !schema.fields.first().is_some_and(|field| {
                    field.value_type.as_ref().is_some_and(|value| {
                        !value.nullable && value.summary() == SyntheticValueKind::Boolean
                    })
                })
            {
                return Err(at(site, "condition requires a boolean value"));
            }
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
            branches.push(id);
            schemas.push(schema);
        }
        let mut schema = schemas.remove(0);
        let left = schema.fields[0]
            .value_type
            .take()
            .ok_or("conditional branch type missing")?;
        let right = schemas[0].fields[0]
            .value_type
            .clone()
            .ok_or("conditional branch type missing")?;
        let joined = plasm_core::value_contract::ValueContract::join(left, right);
        schema.fields[0].value_kind = joined.summary();
        schema.fields[0].value_type = Some(joined);
        let union = self.fresh();
        self.insert(DagNode {
            id: union.clone(),
            expr: String::new(),
            singleton: true,
            page_size: None,
            source: super::super::types::DagNodeSource::Compute {
                source: branches[0].clone(),
                op: ComputeOp::MergeBranches {
                    other: OutputName::new(&branches[1])?,
                },
                schema: schema.clone(),
                collection_alias: None,
            },
        })?;
        let id = union;
        inputs.insert(
            id.clone(),
            PlanDataInput {
                node: id.clone(),
                alias: id.clone(),
                cardinality: InputCardinality::Singleton,
            },
        );
        Ok(PlasmDataValue::NodeSymbol {
            node: id.clone(),
            alias: id,
            path: vec!["value".into()],
        })
    }
}
