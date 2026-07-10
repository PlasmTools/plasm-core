//! Cross-catalog workflow identity: declared keys, conflict taxonomy, idempotent reconcile.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Portable conflict kinds mapped from catalog `conflict_rules`.
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
pub struct ReconcileSpec {
    pub on: WorkflowConflictKind,
    /// Capability name (get/query) that fetches the existing row.
    pub via: String,
    #[serde(default)]
    pub bind_identity_from: ReconcileBindSource,
}

/// Match predicate for one catalog conflict rule (mappings `conflict_rules`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConflictRuleWhen {
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_json_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
}

/// Field extraction from error JSON body (`$.field` JSON pointer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConflictRuleExtract {
    pub entity: String,
    #[serde(default)]
    pub fields: IndexMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConflictRule {
    pub when: ConflictRuleWhen,
    pub kind: WorkflowConflictKind,
    #[serde(default)]
    pub extract: Option<ConflictRuleExtract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
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

pub fn conflict_rules_from_mapping_template(template: &Value) -> Vec<ConflictRule> {
    template
        .get("conflict_rules")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

pub fn json_pointer_get<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() || pointer == "/" {
        return Some(value);
    }
    let path = pointer.trim_start_matches('/').trim_start_matches('$');
    let mut current = value;
    for segment in path.split('/') {
        if segment.is_empty() {
            continue;
        }
        current = if let Ok(index) = segment.parse::<usize>() {
            current.get(index)?
        } else {
            current.get(segment)?
        };
    }
    Some(current)
}

pub fn extract_conflict_fields(body: &Value, fields: &IndexMap<String, String>) -> IndexMap<String, Value> {
    let mut out = IndexMap::new();
    for (name, ptr) in fields {
        if let Some(v) = json_pointer_get(body, ptr) {
            out.insert(name.clone(), value_to_plasm_json(v));
        }
    }
    out
}

fn value_to_plasm_json(v: &Value) -> Value {
    v.clone()
}

pub fn match_conflict_rule(
    rules: &[ConflictRule],
    status: u16,
    body: &Value,
) -> Option<WorkflowConflict> {
    for rule in rules {
        if rule.when.status != status {
            continue;
        }
        if let Some(path) = rule.when.body_json_path.as_deref() {
            let needle = rule.when.contains.as_deref().unwrap_or("");
            let hay = json_pointer_get(body, path)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !hay.contains(needle) {
                continue;
            }
        } else if let Some(needle) = rule.when.contains.as_deref() {
            let body_str = body.to_string();
            if !body_str.contains(needle) {
                continue;
            }
        }
        let key = rule
            .extract
            .as_ref()
            .map(|e| extract_conflict_fields(body, &e.fields))
            .unwrap_or_default();
        let entity = rule
            .extract
            .as_ref()
            .map(|e| e.entity.clone())
            .unwrap_or_else(|| "Resource".to_string());
        let hint = rule.hint.clone().unwrap_or_else(|| {
            format!(
                "{} on {}",
                rule.kind.as_str(),
                entity
            )
        });
        return Some(WorkflowConflict {
            kind: rule.kind,
            entity,
            key,
            hint,
            existing: None,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_conflict_rule_by_status_and_message() {
        let rules = vec![ConflictRule {
            when: ConflictRuleWhen {
                status: 409,
                body_json_path: Some("message".into()),
                contains: Some("already exists".into()),
            },
            kind: WorkflowConflictKind::ResourceExists,
            extract: Some(ConflictRuleExtract {
                entity: "WorkItem".into(),
                fields: IndexMap::from([("title".into(), "/title".into())]),
            }),
            hint: Some("work item title already taken".into()),
        }];
        let body = serde_json::json!({ "message": "title already exists", "title": "foo" });
        let c = match_conflict_rule(&rules, 409, &body).expect("match");
        assert_eq!(c.kind, WorkflowConflictKind::ResourceExists);
        assert_eq!(c.entity, "WorkItem");
        assert_eq!(c.key.get("title").and_then(|v| v.as_str()), Some("foo"));
    }

    #[test]
    fn match_conflict_rule_nested_json_pointer() {
        let rules = vec![ConflictRule {
            when: ConflictRuleWhen {
                status: 200,
                body_json_path: Some("errors/0/message".into()),
                contains: Some("duplicate".into()),
            },
            kind: WorkflowConflictKind::ResourceExists,
            extract: None,
            hint: None,
        }];
        let body = serde_json::json!({ "errors": [{ "message": "duplicate title" }] });
        assert!(match_conflict_rule(&rules, 200, &body).is_some());
    }
}
