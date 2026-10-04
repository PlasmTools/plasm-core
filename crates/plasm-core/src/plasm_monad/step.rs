use super::comp::StepId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    Read,
    Write,
    SideEffect,
    ArtifactRead,
}

/// Resolved effect provenance shared by frontend admission and the plan wire.
/// A returned row or acknowledgement is a separate property of an operation.
pub trait EffectEvidence {
    fn effect_class(&self) -> EffectClass;

    fn is_write_or_side_effect(&self) -> bool {
        matches!(
            self.effect_class(),
            EffectClass::Write | EffectClass::SideEffect
        )
    }
}

impl EffectEvidence for EffectClass {
    fn effect_class(&self) -> EffectClass {
        *self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultShape {
    List,
    Single,
    MutationResult,
    SideEffectAck,
    Page,
    Artifact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    Query,
    Search,
    Get,
    Create,
    Update,
    Delete,
    Action,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlasmStepKind {
    Invoke,
    Pure,
    Map,
    Derive,
    FlatMapRelation,
    FlatMapApply,
    /// PLP-8 state iterator (`iterate … until … take N`).
    UnfoldUntil,
    MapBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlasmStep {
    Invoke {
        id: StepId,
        surface: SurfaceKind,
        effect: EffectClass,
        shape: ResultShape,
        operation: String,
    },
    Pure {
        id: StepId,
        shape: ResultShape,
        summary: String,
    },
    Map {
        id: StepId,
        source: StepId,
        op: String,
        shape: ResultShape,
    },
    Derive {
        id: StepId,
        source: StepId,
        template: String,
        shape: ResultShape,
    },
    FlatMapRelation {
        id: StepId,
        source: StepId,
        relation: String,
        operation: String,
        shape: ResultShape,
    },
    FlatMapApply {
        id: StepId,
        source: StepId,
        effect: EffectClass,
        operation: String,
        shape: ResultShape,
    },
    UnfoldUntil {
        id: StepId,
        source: StepId,
        effect: SurfaceKind,
        operation: String,
        shape: ResultShape,
        take: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectBarrier {
    Read,
    Write,
    SideEffect,
    Approval,
}

impl PlasmStep {
    pub fn id(&self) -> &StepId {
        match self {
            Self::Invoke { id, .. }
            | Self::Pure { id, .. }
            | Self::Map { id, .. }
            | Self::Derive { id, .. }
            | Self::FlatMapRelation { id, .. }
            | Self::FlatMapApply { id, .. }
            | Self::UnfoldUntil { id, .. } => id,
        }
    }

    pub fn kind(&self) -> PlasmStepKind {
        match self {
            Self::Invoke { .. } => PlasmStepKind::Invoke,
            Self::Pure { .. } => PlasmStepKind::Pure,
            Self::Map { .. } => PlasmStepKind::Map,
            Self::Derive { .. } => PlasmStepKind::Derive,
            Self::FlatMapRelation { .. } => PlasmStepKind::FlatMapRelation,
            Self::FlatMapApply { .. } => PlasmStepKind::FlatMapApply,
            Self::UnfoldUntil { .. } => PlasmStepKind::UnfoldUntil,
        }
    }

    pub fn effect_barrier(&self) -> EffectBarrier {
        match self {
            Self::Invoke { effect, .. } => match effect {
                EffectClass::Read | EffectClass::ArtifactRead => EffectBarrier::Read,
                EffectClass::Write => EffectBarrier::Write,
                EffectClass::SideEffect => EffectBarrier::SideEffect,
            },
            Self::FlatMapApply { effect, .. } => match effect {
                EffectClass::Read | EffectClass::ArtifactRead => EffectBarrier::Read,
                EffectClass::Write => EffectBarrier::Write,
                EffectClass::SideEffect => EffectBarrier::SideEffect,
            },
            Self::UnfoldUntil { .. } => EffectBarrier::Write,
            Self::Pure { .. }
            | Self::Map { .. }
            | Self::Derive { .. }
            | Self::FlatMapRelation { .. } => EffectBarrier::Read,
        }
    }

    pub fn is_pure_step(&self) -> bool {
        matches!(
            self,
            Self::Pure { .. } | Self::Map { .. } | Self::Derive { .. }
        )
    }
}
