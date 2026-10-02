//! Quantification lowers through ordinary scoped selection and complete reduction.
use super::*;
use crate::plasm_plan::{InputCardinality, PlanDataInput};
use ruff_python_ast::{ExprCall, ExprLambda, Parameter, ParameterWithDefault, Parameters};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuantifierOperation {
    Any,
    All,
}
impl QuantifierOperation {
    pub const ALL: &'static [Self] = &[Self::Any, Self::All];
    pub fn name(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::All => "all",
        }
    }
    pub(super) fn parse(name: &str) -> Option<Self> {
        match name {
            "any" => Some(Self::Any),
            "all" => Some(Self::All),
            _ => None,
        }
    }
}

impl Lower<'_> {
    pub(super) fn quantify(
        &mut self,
        site: &PyExpr,
        call: &ExprCall,
        all: bool,
        inputs: &mut BTreeMap<String, PlanDataInput>,
    ) -> Result<PlasmDataValue, String> {
        let [argument] = call.arguments.args.as_ref() else {
            return Err(at(site, "quantification requires one iterable"));
        };
        if !call.arguments.keywords.is_empty() {
            return Err(at(site, "quantification does not accept keywords"));
        }
        let PyExpr::Generator(generator) = argument else {
            return self.python_value_expression(site, inputs);
        };
        if generator.generators.is_empty() {
            return Err(at(
                site,
                "quantification requires one synchronous generator",
            ));
        }
        let clause = &generator.generators[0];
        let PyExpr::Name(parameter) = &clause.target else {
            return Err(at(site, "generator target must be a row name"));
        };
        if clause.is_async {
            return Err(at(site, "async generators are not outer DAG expressions"));
        }
        let root = expression_root(&clause.iter);
        // Value constructors stay in the recursive value algebra. Lifting a
        // literal array into a DAG node changes lazy evaluation into an eager
        // dependency and needlessly makes its enclosing position significant.
        let local_value = matches!(&clause.iter, PyExpr::Tuple(_))
            || (super::value_expressions::is_value_expression(&clause.iter)
                && self.expression_placement(&clause.iter) != ExpressionPlacement::Host)
            || root.is_some_and(|root| self.frame.quantifiers.contains_key(root));
        let source = if local_value {
            None
        } else {
            Some(self.expr(&clause.iter, None)?)
        };
        let scalar = source.as_ref().is_some_and(|id| {
            super::super::binding_contract(&self.state, id)
                .is_some_and(|c| c.value_kind == BindingValueKind::ScalarCell)
        });
        let mut test = *generator.elt.clone();
        if generator.generators.len() > 1 {
            let mut nested = generator.clone();
            nested.generators.remove(0);
            let mut nested_call = call.clone();
            nested_call.arguments.args = vec![PyExpr::Generator(nested)].into();
            test = PyExpr::Call(nested_call);
        }
        if !clause.ifs.is_empty() {
            let condition = PyExpr::BoolOp(ruff_python_ast::ExprBoolOp {
                node_index: Default::default(),
                range: site.range(),
                op: ruff_python_ast::BoolOp::And,
                values: clause.ifs.clone(),
            });
            test = PyExpr::If(ruff_python_ast::ExprIf {
                node_index: Default::default(),
                range: site.range(),
                test: Box::new(condition),
                body: Box::new(test),
                orelse: Box::new(PyExpr::BooleanLiteral(
                    ruff_python_ast::ExprBooleanLiteral {
                        node_index: Default::default(),
                        range: site.range(),
                        value: all,
                    },
                )),
            });
        }
        if local_value || scalar {
            return self.python_value_expression(site, inputs);
        }

        let source = source.ok_or("quantifier source missing")?;
        let lambda = PyExpr::Lambda(ExprLambda {
            node_index: Default::default(),
            range: site.range(),
            parameters: Some(Box::new(Parameters {
                node_index: Default::default(),
                range: clause.target.range(),
                posonlyargs: Default::default(),
                kwonlyargs: Default::default(),
                vararg: None,
                kwarg: None,
                args: vec![ParameterWithDefault {
                    node_index: Default::default(),
                    range: clause.target.range(),
                    default: None,
                    parameter: Parameter {
                        node_index: Default::default(),
                        range: clause.target.range(),
                        name: ruff_python_ast::Identifier::new(
                            parameter.id.clone(),
                            parameter.range(),
                        ),
                        annotation: None,
                    },
                }]
                .into_iter()
                .collect(),
            })),
            body: Box::new(test),
        });
        let PyExpr::Lambda(lambda) = lambda else {
            unreachable!()
        };
        let body = self.scoped_body(
            site,
            &source,
            &lambda,
            std::num::NonZeroU32::new(65_536).expect("positive bound"),
            super::body::ScopeMode::Quantify(all),
        )?;
        let schema = crate::map_body_schema::output_schema(self.es, &body)?;
        let id = self.fresh();
        self.insert(DagNode {
            id: id.clone(),
            expr: String::new(),
            singleton: true,
            page_size: None,
            source: super::super::types::DagNodeSource::MapBody {
                body: Box::new(body),
                schema,
            },
        })?;
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
            path: vec!["predicate".into()],
        })
    }
}

pub(super) fn expression_root(expr: &PyExpr) -> Option<&str> {
    match expr {
        PyExpr::Name(n) => Some(n.id.as_str()),
        PyExpr::Attribute(a) => expression_root(&a.value),
        _ => None,
    }
}
