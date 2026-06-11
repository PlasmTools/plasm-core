//! Tenant workflow manifest — semantic binds only (never session symbols).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::workflow_program_template::{parse_program_template, WorkflowProgramTemplate};

pub const WORKFLOW_MANIFEST_SCHEMA_VERSION: u32 = 1;

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
    pub fn parsed_template(&self) -> Result<WorkflowProgramTemplate, crate::workflow_program_template::TemplateParseError> {
        parse_program_template(&self.program_template)
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
