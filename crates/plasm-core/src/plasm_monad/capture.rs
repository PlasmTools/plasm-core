//! Scope ports share one value/receiver contract without conflating cardinality.
use super::{PlanQualifiedEntityKey, StepId, SyntheticResultSchema};
use crate::value_contract::ValueContract;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CaptureContract {
    Value {
        value: ValueContract,
    },
    Rows {
        schema: Option<SyntheticResultSchema>,
        entity_authority: bool,
    },
}

impl CaptureContract {
    pub fn value_contract(&self) -> Option<&ValueContract> {
        match self {
            Self::Value { value } => Some(value),
            Self::Rows { .. } => None,
        }
    }
    pub fn schema(&self) -> Option<&SyntheticResultSchema> {
        match self {
            Self::Rows { schema, .. } => schema.as_ref(),
            Self::Value { .. } => None,
        }
    }
    pub fn entity_authority(&self) -> bool {
        matches!(
            self,
            Self::Rows {
                entity_authority: true,
                ..
            }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureCardinality {
    ParentOccurrence,
    Singleton,
    Collection,
}

pub trait CapturePort {
    fn source(&self) -> &StepId;
    fn local(&self) -> &StepId;
    fn entity(&self) -> &PlanQualifiedEntityKey;
    fn contract(&self) -> &CaptureContract;
    fn cardinality(&self) -> CaptureCardinality;
}

#[cfg(test)]
mod tests {
    #[test]
    fn prior_capture_wire_version_is_rejected() {
        let mut comp = super::super::empty_comp(None);
        comp.version = 2;
        assert!(matches!(
            comp.validate(),
            Err(super::super::PlasmCompValidationError::UnsupportedVersion {
                expected: 3,
                actual: 2
            })
        ));
    }
}
