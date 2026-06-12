use super::atoms::PlanQualifiedEntityKey;
use super::compute::ComputeTemplate;
use super::expr::{PlanExprIr, PlanExprTemplate};
use super::relation::PlanRelationTraversal;
use super::templates::{DeriveTemplate, EffectTemplate};
use super::value::{PlanPredicate, PlasmDataValue};
use crate::plasm_monad::step::{EffectClass, PlasmStepKind, ResultShape, SurfaceKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvokePayload {
    pub plan_kind: SurfaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified_entity: Option<PlanQualifiedEntityKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ir: Option<PlanExprIr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ir_template: Option<PlanExprTemplate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projection: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predicates: Vec<PlanPredicate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_expr: Option<String>,
    pub effect_class: EffectClass,
    pub result_shape: ResultShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurePayload {
    pub data: PlasmDataValue,
    pub effect_class: EffectClass,
    pub result_shape: ResultShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapPayload {
    pub compute: ComputeTemplate,
    pub effect_class: EffectClass,
    pub result_shape: ResultShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivePayload {
    pub derive: DeriveTemplate,
    pub effect_class: EffectClass,
    pub result_shape: ResultShape,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlatMapRelationPayload {
    pub relation: PlanRelationTraversal,
    pub effect_class: EffectClass,
    pub result_shape: ResultShape,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlatMapEffectPayload {
    pub source: String,
    pub item_binding: super::atoms::BindingName,
    pub effect_template: EffectTemplate,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projection: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predicates: Vec<PlanPredicate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<String>,
    pub effect_class: EffectClass,
    pub result_shape: ResultShape,
}

/// Typed step payload in a [`super::super::comp::PlasmComp`] DAG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlasmStepPayload {
    Invoke(InvokePayload),
    Pure(PurePayload),
    Map(MapPayload),
    Derive(DerivePayload),
    FlatMapRelation(FlatMapRelationPayload),
    FlatMapEffect(FlatMapEffectPayload),
}

impl PlasmStepPayload {
    pub fn kind(&self) -> PlasmStepKind {
        match self {
            Self::Invoke { .. } => PlasmStepKind::Invoke,
            Self::Pure { .. } => PlasmStepKind::Pure,
            Self::Map { .. } => PlasmStepKind::Map,
            Self::Derive { .. } => PlasmStepKind::Derive,
            Self::FlatMapRelation { .. } => PlasmStepKind::FlatMapRelation,
            Self::FlatMapEffect { .. } => PlasmStepKind::FlatMapEffect,
        }
    }

    pub fn effect_class(&self) -> EffectClass {
        match self {
            Self::Invoke(p) => p.effect_class,
            Self::Pure(p) => p.effect_class,
            Self::Map(p) => p.effect_class,
            Self::Derive(p) => p.effect_class,
            Self::FlatMapRelation(p) => p.effect_class,
            Self::FlatMapEffect(p) => p.effect_class,
        }
    }

    pub fn result_shape(&self) -> ResultShape {
        match self {
            Self::Invoke(p) => p.result_shape,
            Self::Pure(p) => p.result_shape,
            Self::Map(p) => p.result_shape,
            Self::Derive(p) => p.result_shape,
            Self::FlatMapRelation(p) => p.result_shape,
            Self::FlatMapEffect(p) => p.result_shape,
        }
    }

    pub fn operation_display(&self) -> String {
        match self {
            Self::Invoke(p) => p
                .display_expr
                .clone()
                .or_else(|| {
                    p.qualified_entity
                        .as_ref()
                        .map(|qe| format!("{} {}", surface_label(p.plan_kind), qe.entity))
                })
                .unwrap_or_else(|| surface_label(p.plan_kind)),
            Self::Pure(_) => "pure".into(),
            Self::Map(p) => compute_op_label(&p.compute.op),
            Self::Derive(p) => format!("derive {}", derive_kind_label(p.derive.kind)),
            Self::FlatMapRelation(p) => format!("relation {}", p.relation.relation),
            Self::FlatMapEffect(p) => format!("for_each {}", surface_label(p.effect_template.kind)),
        }
    }
}

fn surface_label(k: SurfaceKind) -> String {
    match k {
        SurfaceKind::Query => "query".into(),
        SurfaceKind::Search => "search".into(),
        SurfaceKind::Get => "get".into(),
        SurfaceKind::Create => "create".into(),
        SurfaceKind::Update => "update".into(),
        SurfaceKind::Delete => "delete".into(),
        SurfaceKind::Action => "action".into(),
    }
}

fn derive_kind_label(k: super::templates::DeriveKind) -> &'static str {
    match k {
        super::templates::DeriveKind::Map => "map",
        super::templates::DeriveKind::Data => "data",
    }
}

fn compute_op_label(op: &super::compute::ComputeOp) -> String {
    use super::compute::ComputeOp;
    match op {
        ComputeOp::Project { .. } => "project".into(),
        ComputeOp::Filter { .. } => "filter".into(),
        ComputeOp::GroupBy { .. } => "group_by".into(),
        ComputeOp::Aggregate { .. } => "aggregate".into(),
        ComputeOp::Sort { .. } => "sort".into(),
        ComputeOp::Limit { count } => format!("limit {count}"),
        ComputeOp::DedupeBy { .. } => "dedupe_by".into(),
        ComputeOp::Render { .. } => "render".into(),
    }
}
