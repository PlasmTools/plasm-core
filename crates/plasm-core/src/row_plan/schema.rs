//! Logical frame schema. Physical Polars dtypes stay behind the runtime adapter.

use crate::identity::EntityName;
use crate::plasm_monad::payload::FieldPath;
use crate::TemporalWireFormat;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::plasm_monad::OutputName;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ColumnName(OutputName);

impl ColumnName {
    pub fn new(name: impl Into<String>) -> Result<Self, String> {
        Ok(Self(OutputName::new(name.into())?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[must_use]
    pub fn as_output_name(&self) -> &OutputName {
        &self.0
    }
}

impl From<OutputName> for ColumnName {
    fn from(name: OutputName) -> Self {
        Self(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlasmFrameSchema {
    shape: FrameShape,
    columns: IndexMap<String, LogicalColumn>,
}

impl PlasmFrameSchema {
    #[must_use]
    pub fn new(shape: FrameShape, columns: IndexMap<String, LogicalColumn>) -> Self {
        Self { shape, columns }
    }

    #[must_use]
    pub fn opaque_object() -> Self {
        Self {
            shape: FrameShape::Remapped {
                reason: RemapReason::Project,
            },
            columns: IndexMap::new(),
        }
    }

    #[must_use]
    pub fn shape(&self) -> &FrameShape {
        &self.shape
    }

    #[must_use]
    pub fn columns(&self) -> &IndexMap<String, LogicalColumn> {
        &self.columns
    }

    #[must_use]
    pub fn with_intact_identity(mut self) -> Self {
        if let FrameShape::Entity { identity, .. } = &mut self.shape {
            *identity = IdentityPreservation::Intact;
        }
        self
    }

    pub fn insert_column(&mut self, name: ColumnName, col: LogicalColumn) {
        self.columns.insert(name.as_str().to_string(), col);
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicalColumn {
    pub ty: LogicalColumnType,
    pub nullable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<FieldPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogicalColumnType {
    Null,
    Boolean,
    Integer,
    Number,
    String,
    Duration,
    Temporal { format: TemporalWireFormat },
    Money { currency: MoneyColumnLayout },
    EntityRef { target: EntityName },
    Array,
    Object,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoneyColumnLayout {
    Uniform { currency: String },
    PerRow,
}
