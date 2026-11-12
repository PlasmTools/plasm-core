//! Human-readable diagnostics for typed teaching contracts.
use super::{PreservationCategory, PreservationPath, SemanticReference};
use std::fmt;

impl fmt::Display for SemanticReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValueDomain {
                entry_id,
                value_ref,
            } => {
                write!(f, "value domain `{value_ref}` in catalogue `{entry_id}`")
            }
            Self::Entity { entry_id, entity } => {
                write!(f, "entity `{entity}` in catalogue `{entry_id}`")
            }
            Self::Method {
                entry_id,
                entity,
                capability,
            } => {
                write!(
                    f,
                    "capability `{capability}` on entity `{entity}` in catalogue `{entry_id}`"
                )
            }
            Self::Relation {
                entry_id,
                entity,
                relation,
            } => {
                write!(
                    f,
                    "relation `{relation}` on entity `{entity}` in catalogue `{entry_id}`"
                )
            }
        }
    }
}

impl fmt::Display for PreservationCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ApprovalGates => "approval gates changed",
            Self::CanonicalComputation => "canonical computation changed",
        })
    }
}

pub(super) struct PreservationLocation<'a>(pub &'a [PreservationPath]);

impl fmt::Display for PreservationLocation<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("$")?;
        for component in self.0 {
            match component {
                PreservationPath::Field(name) => write!(f, ".{name}")?,
                PreservationPath::Index(index) => write!(f, "[{index}]")?,
            }
        }
        Ok(())
    }
}
