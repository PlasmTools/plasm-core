//! Recursive row declarations and identity preservation at engine boundaries.
use crate::{
    identity::EntityName,
    value_contract::{ValueContract, ValueShape},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlasmFrameSchema {
    shape: FrameShape,
    contract: ValueContract,
}
impl PlasmFrameSchema {
    pub fn new(shape: FrameShape, contract: ValueContract) -> Result<Self, String> {
        if !matches!(
            contract.shape,
            ValueShape::Record { .. } | ValueShape::ObservedRecord { .. }
        ) {
            return Err("frame requires a record contract".into());
        }
        Ok(Self { shape, contract })
    }
    pub fn contract(&self) -> &ValueContract {
        &self.contract
    }
    pub fn shape(&self) -> &FrameShape {
        &self.shape
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameShape {
    Entity {
        entity: EntityName,
        identity: IdentityPreservation,
    },
    Remapped {
        reason: RemapReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityPreservation {
    Intact,
    Projected,
    Aggregated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemapReason {
    Project,
    GroupBy,
    Aggregate,
    Derive,
}
