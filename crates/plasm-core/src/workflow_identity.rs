//! Cross-catalog workflow identity: declared keys, conflict taxonomy, idempotent reconcile.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Conflicts established by local identity and authoritative read checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowConflictKind {
    ResourceExists,
    IdentityMismatch,
}

/// Structured conflict surfaced to agents (MCP / plan review).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowConflict {
    pub kind: WorkflowConflictKind,
    pub entity: String,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub key: IndexMap<String, Value>,
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing: Option<IndexMap<String, Value>>,
}

impl WorkflowConflict {
    pub fn markdown_block(&self) -> String {
        let mut lines = vec![
            format!("**workflow conflict** · `{}`", self.kind.as_str()),
            format!("entity: `{}`", self.entity),
        ];
        if !self.key.is_empty() {
            if let Ok(json) = serde_json::to_string(&self.key) {
                lines.push(format!("key: `{json}`"));
            }
        }
        lines.push(format!("hint: {}", self.hint));
        if let Some(existing) = &self.existing {
            if let Ok(json) = serde_json::to_string(existing) {
                lines.push(format!("existing: `{json}`"));
            }
        }
        lines.join("\n")
    }
}

impl WorkflowConflictKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResourceExists => "resource_exists",
            Self::IdentityMismatch => "identity_mismatch",
        }
    }
}

/// Where reconcile binds identity fields from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileBindSource {
    #[default]
    Params,
    Scope,
}

/// Idempotent reconcile policy on [`crate::OutputSchema`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileSpec {
    /// Capability name (get/query) that fetches the existing row.
    pub via: String,
    #[serde(default)]
    pub bind_identity_from: ReconcileBindSource,
}

/// Conditional execution guard on a view DAG node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewNodeWhen {
    SkipIf { condition: ViewNodeCondition },
    RunIf { condition: ViewNodeCondition },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ViewNodeCondition {
    NodeRowCountPositive { node: String },
    NodeRowCountZero { node: String },
}

/// Standard write outcome on entity projections (`created` | `reused`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteOutcome {
    Created,
    Reused,
    Skipped,
}
