//! Validate project flow policy against catalog vocabulary.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;

use crate::flow_policy_vocabulary::ProjectFlowVocabulary;
use crate::plan_flow_policy::FlowPolicy;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FlowPolicyDiagnostic {
    pub severity: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_pointer: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FlowPolicyValidateResult {
    pub ok: bool,
    pub errors: Vec<FlowPolicyDiagnostic>,
    pub warnings: Vec<FlowPolicyDiagnostic>,
}

pub fn validate_flow_policy(
    policy: &FlowPolicy,
    vocab: &ProjectFlowVocabulary,
) -> FlowPolicyValidateResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let label_catalogs = label_to_catalogs(vocab);
    let all_labels: HashSet<&str> = label_catalogs.keys().copied().collect();
    let sink_catalogs = sink_to_catalogs(vocab);
    let all_sinks: HashSet<&str> = sink_catalogs.keys().copied().collect();
    let enabled_entries: HashSet<&str> =
        vocab.catalogs.iter().map(|c| c.entry_id.as_str()).collect();
    let entities_by_entry: HashMap<&str, HashSet<&str>> = vocab
        .catalogs
        .iter()
        .map(|c| {
            (
                c.entry_id.as_str(),
                c.entities.iter().map(|s| s.as_str()).collect(),
            )
        })
        .collect();

    for (idx, rule) in policy.forbidden.iter().enumerate() {
        let label = rule.from_label.as_str();
        if !all_labels.contains(label) {
            errors.push(FlowPolicyDiagnostic {
                severity: "error".into(),
                code: "unknown_label".into(),
                message: format!(
                    "Label `{label}` is not declared in any enabled catalog — enable the catalog or remove this rule."
                ),
                rule_index: Some(idx),
                field: Some("from_label".into()),
                token: Some(label.to_string()),
                json_pointer: Some(format!("/forbidden/{idx}/from_label")),
            });
        }
        if let Some(sink) = rule.to_sink.as_ref() {
            let sink_s = sink.as_str();
            if !all_sinks.contains(sink_s) {
                errors.push(FlowPolicyDiagnostic {
                    severity: "error".into(),
                    code: "unknown_sink".into(),
                    message: format!(
                        "Sink `{sink_s}` is not declared in any enabled catalog capability."
                    ),
                    rule_index: Some(idx),
                    field: Some("to_sink".into()),
                    token: Some(sink_s.to_string()),
                    json_pointer: Some(format!("/forbidden/{idx}/to_sink")),
                });
            }
        }
        if rule.reason.as_ref().is_none_or(|r| r.trim().is_empty()) {
            errors.push(FlowPolicyDiagnostic {
                severity: "error".into(),
                code: "missing_reason".into(),
                message: "Forbidden flow rules require a human-readable reason.".into(),
                rule_index: Some(idx),
                field: Some("reason".into()),
                token: None,
                json_pointer: Some(format!("/forbidden/{idx}/reason")),
            });
        }
    }

    for (idx, rule) in policy.effect_rules.iter().enumerate() {
        if let Some(entry_id) = rule.pattern.entry_id.as_ref() {
            if !enabled_entries.contains(entry_id.as_str()) {
                errors.push(FlowPolicyDiagnostic {
                    severity: "error".into(),
                    code: "unknown_entry_id".into(),
                    message: format!(
                        "Effect rule references catalog `{entry_id}` which is not enabled on any MCP bundle."
                    ),
                    rule_index: Some(idx),
                    field: Some("pattern.entry_id".into()),
                    token: Some(entry_id.clone()),
                    json_pointer: Some(format!("/effect_rules/{idx}/pattern/entry_id")),
                });
            }
        }
        if let (Some(entry_id), Some(entity)) =
            (rule.pattern.entry_id.as_ref(), rule.pattern.entity.as_ref())
        {
            if let Some(entities) = entities_by_entry.get(entry_id.as_str()) {
                if !entities.contains(entity.as_str()) {
                    errors.push(FlowPolicyDiagnostic {
                        severity: "error".into(),
                        code: "unknown_entity".into(),
                        message: format!("Entity `{entity}` is not in catalog `{entry_id}`."),
                        rule_index: Some(idx),
                        field: Some("pattern.entity".into()),
                        token: Some(entity.clone()),
                        json_pointer: Some(format!("/effect_rules/{idx}/pattern/entity")),
                    });
                }
            }
        }
    }

    for san in &policy.sanitizers {
        for label in &san.clears {
            let l = label.as_str();
            if !all_labels.contains(l) {
                errors.push(FlowPolicyDiagnostic {
                    severity: "error".into(),
                    code: "unknown_sanitizer_label".into(),
                    message: format!("Sanitizer clears unknown label `{l}`."),
                    rule_index: None,
                    field: Some("clears".into()),
                    token: Some(l.to_string()),
                    json_pointer: None,
                });
            }
        }
    }

    detect_duplicate_forbidden(policy, &mut warnings);
    detect_broad_effect_rules(policy, &mut warnings);

    FlowPolicyValidateResult {
        ok: errors.is_empty(),
        errors,
        warnings,
    }
}

fn label_to_catalogs(vocab: &ProjectFlowVocabulary) -> HashMap<&str, Vec<&str>> {
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for cat in &vocab.catalogs {
        for dc in &cat.data_classes {
            out.entry(dc.id.as_str())
                .or_default()
                .push(cat.entry_id.as_str());
        }
    }
    out
}

fn sink_to_catalogs(vocab: &ProjectFlowVocabulary) -> HashMap<&str, Vec<&str>> {
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for cat in &vocab.catalogs {
        for sink in &cat.sink_classes {
            out.entry(sink.as_str())
                .or_default()
                .push(cat.entry_id.as_str());
        }
    }
    out
}

fn detect_duplicate_forbidden(policy: &FlowPolicy, warnings: &mut Vec<FlowPolicyDiagnostic>) {
    let mut seen: BTreeSet<(String, Option<String>)> = BTreeSet::new();
    for (idx, rule) in policy.forbidden.iter().enumerate() {
        let key = (
            rule.from_label.as_str().to_string(),
            rule.to_sink.as_ref().map(|s| s.as_str().to_string()),
        );
        if !seen.insert(key.clone()) {
            warnings.push(FlowPolicyDiagnostic {
                severity: "warning".into(),
                code: "duplicate_forbidden".into(),
                message: format!(
                    "Duplicate forbidden flow ({}{}) — only the first match is meaningful.",
                    key.0,
                    key.1
                        .as_ref()
                        .map(|s| format!(" → {s}"))
                        .unwrap_or_default()
                ),
                rule_index: Some(idx),
                field: None,
                token: None,
                json_pointer: Some(format!("/forbidden/{idx}")),
            });
        }
    }
}

fn detect_broad_effect_rules(policy: &FlowPolicy, warnings: &mut Vec<FlowPolicyDiagnostic>) {
    for (idx, rule) in policy.effect_rules.iter().enumerate() {
        let p = &rule.pattern;
        let is_broad = p.entry_id.is_none()
            && p.entity.is_none()
            && p.operation.is_some()
            && matches!(
                rule.enforcement,
                crate::plan_flow_policy::FlowEnforcement::Deny
                    | crate::plan_flow_policy::FlowEnforcement::Review
            );
        if is_broad {
            warnings.push(FlowPolicyDiagnostic {
                severity: "warning".into(),
                code: "broad_effect_rule".into(),
                message: "Broad effect rule matches all catalogs — narrower rules below may never apply (first match wins).".into(),
                rule_index: Some(idx),
                field: None,
                token: None,
                json_pointer: Some(format!("/effect_rules/{idx}")),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_policy_vocabulary::CatalogVocabulary;
    use crate::plan_flow_policy::{FlowPolicy, ForbiddenFlowRule};
    use plasm_core::{DataClassName, SinkClassName};

    fn sample_vocab() -> ProjectFlowVocabulary {
        ProjectFlowVocabulary {
            catalogs: vec![CatalogVocabulary {
                entry_id: "vultr".into(),
                catalog_has_labels: true,
                data_classes: vec![crate::flow_policy_vocabulary::DataClassVocabEntry {
                    id: "credentials".into(),
                    severity: "critical".into(),
                    description: None,
                }],
                sink_classes: vec!["external_publish".into()],
                entities: vec!["Instance".into()],
            }],
        }
    }

    #[test]
    fn rejects_unknown_label() {
        let policy = FlowPolicy {
            forbidden: vec![ForbiddenFlowRule {
                from_label: DataClassName::new("made_up").unwrap(),
                to_sink: Some(SinkClassName::new("external_publish").unwrap()),
                reason: Some("test".into()),
            }],
            effect_rules: vec![],
            sanitizers: vec![],
        };
        let r = validate_flow_policy(&policy, &sample_vocab());
        assert!(!r.ok);
        assert!(r.errors.iter().any(|e| e.code == "unknown_label"));
    }

    #[test]
    fn accepts_known_label_and_sink() {
        let policy = FlowPolicy {
            forbidden: vec![ForbiddenFlowRule {
                from_label: DataClassName::new("credentials").unwrap(),
                to_sink: Some(SinkClassName::new("external_publish").unwrap()),
                reason: Some("no secrets in tickets".into()),
            }],
            effect_rules: vec![],
            sanitizers: vec![],
        };
        let r = validate_flow_policy(&policy, &sample_vocab());
        assert!(r.ok, "{r:?}");
    }
}
