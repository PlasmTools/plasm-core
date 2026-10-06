//! Tenant workflow manifest — semantic binds only (never session symbols).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::workflow_program_template::{parse_program_template, WorkflowProgramTemplate};

pub const WORKFLOW_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowManifestError {
    #[error("workflow manifest schema_version must be {expected} (got {actual})")]
    SchemaVersion { expected: u32, actual: u32 },
    #[error("workflow manifest id missing")]
    MissingId,
    #[error("workflow manifest title missing")]
    MissingTitle,
    #[error("workflow manifest program_template missing")]
    MissingProgramTemplate,
    #[error("workflow manifest seeds must be non-empty")]
    EmptySeeds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowBindKind {
    CapabilityParam,
    EntityField,
    EntityRef,
    TemplateString,
    ProgramBinding,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowSemanticBind {
    pub kind: WorkflowBindKind,
    pub entry_id: String,
    pub entity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowParameter {
    pub name: String,
    pub description: String,
    pub bind: WorkflowSemanticBind,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowSeed {
    pub entry_id: String,
    pub entity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowManifest {
    pub schema_version: u32,
    pub id: String,
    pub title: String,
    pub description: String,
    pub program_template: String,
    pub seeds: Vec<WorkflowSeed>,
    pub parameters: Vec<WorkflowParameter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catalog_pins: Vec<String>,
}

impl WorkflowManifest {
    pub fn parsed_template(
        &self,
    ) -> Result<WorkflowProgramTemplate, crate::workflow_program_template::TemplateParseError> {
        parse_program_template(&self.program_template)
    }

    /// Reject stale or partial manifest wire (exact schema cutover).
    pub fn validate(&self) -> Result<(), WorkflowManifestError> {
        if self.schema_version != WORKFLOW_MANIFEST_SCHEMA_VERSION {
            return Err(WorkflowManifestError::SchemaVersion {
                expected: WORKFLOW_MANIFEST_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        if self.id.trim().is_empty() {
            return Err(WorkflowManifestError::MissingId);
        }
        if self.title.trim().is_empty() {
            return Err(WorkflowManifestError::MissingTitle);
        }
        if self.program_template.trim().is_empty() {
            return Err(WorkflowManifestError::MissingProgramTemplate);
        }
        if self.seeds.is_empty() {
            return Err(WorkflowManifestError::EmptySeeds);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstantiateRequest {
    pub parameters: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstantiateResponse {
    pub program: String,
}
