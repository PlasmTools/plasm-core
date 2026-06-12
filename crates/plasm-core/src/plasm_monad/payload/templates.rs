use super::atoms::{BindingName, PlanQualifiedEntityKey};
use super::expr::PlanExprTemplate;
use crate::plasm_monad::step::{EffectClass, ResultShape, SurfaceKind};
use super::value::{PlanDataInput, PlasmDataValue};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectTemplate {
    pub kind: SurfaceKind,
    pub qualified_entity: PlanQualifiedEntityKey,
    pub expr_template: String,
    pub ir_template: PlanExprTemplate,
    pub effect_class: EffectClass,
    pub result_shape: ResultShape,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projection: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_bindings: Vec<super::value::PlanInputBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeriveTemplate {
    pub kind: DeriveKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_binding: Option<BindingName>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<PlanDataInput>,
    pub value: PlasmDataValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeriveKind {
    Map,
    Data,
}
