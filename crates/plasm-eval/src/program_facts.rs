//! Semantic scoring traverses the compiled DAG, including nested bodies and effects.
use plasm_core::plasm_monad::*;
use std::collections::HashSet;

#[derive(Default)]
pub(crate) struct ProgramFacts {
    pub expressions: Vec<plasm_core::Expr>,
    pub entities: HashSet<String>,
    pub fields: HashSet<String>,
    pub values: HashSet<String>,
    pub relations: HashSet<String>,
    pub projections: Vec<Vec<String>>,
    pub steps: usize,
}
impl ProgramFacts {
    fn predicates<'a>(&mut self, predicates: impl IntoIterator<Item = &'a PlanPredicate>) {
        for predicate in predicates {
            self.fields.insert(predicate.field_path.dotted());
            if let PlasmDataValue::Literal { value } = &predicate.value {
                self.values.insert(
                    serde_json::to_string(&value.clone().into_value()).expect("validated literal"),
                );
            }
        }
    }
    pub fn visit(&mut self, comp: &PlasmComp) {
        self.steps += comp.steps.len();
        for payload in comp.steps.values() {
            match payload {
                PlasmStepPayload::Invoke(p) => {
                    if let Some(owner) = &p.qualified_entity {
                        self.entities.insert(owner.entity.clone());
                    }
                    if let Some(ir) = &p.ir {
                        self.expressions.push(ir.expr.clone());
                        self.projections.extend(ir.projection.clone());
                    }
                    if let Some(ir) = &p.ir_template {
                        self.expressions.push(ir.expr.clone());
                        self.projections.extend(ir.projection.clone());
                    }
                    self.projections.push(p.projection.clone());
                    self.predicates(&p.predicates);
                }
                PlasmStepPayload::FlatMapRelation(p) => {
                    self.entities.insert(p.relation.target.entity.clone());
                    self.relations.insert(p.relation.relation.clone());
                    self.expressions.push(p.relation.ir.expr.clone());
                    self.projections.extend(p.relation.ir.projection.clone());
                }
                PlasmStepPayload::FlatMapApply(p) => {
                    self.entities
                        .insert(p.effect_template.qualified_entity.entity.clone());
                    self.expressions
                        .push(p.effect_template.ir_template.expr.clone());
                    self.projections.push(p.projection.clone());
                    self.predicates(&p.predicates);
                }
                PlasmStepPayload::UnfoldUntil(p) => {
                    self.entities
                        .insert(p.effect_template.qualified_entity.entity.clone());
                    self.expressions
                        .push(p.effect_template.ir_template.expr.clone());
                    if let Some(ir) = &p.seed_ir {
                        self.expressions.push(ir.expr.clone());
                    }
                    self.predicates(&p.until_predicates);
                }
                PlasmStepPayload::Map(p) => match &p.compute.op {
                    ComputeOp::Project { fields } => self
                        .projections
                        .push(fields.keys().map(ToString::to_string).collect()),
                    ComputeOp::Filter { predicates } => self.predicates(predicates.iter()),
                    _ => {}
                },
                PlasmStepPayload::MapBody(p) => {
                    self.entities.insert(p.parent.entity.entity.clone());
                    self.visit(&p.body);
                }
                PlasmStepPayload::Pure(_) | PlasmStepPayload::Derive(_) => {}
            }
        }
    }
}
