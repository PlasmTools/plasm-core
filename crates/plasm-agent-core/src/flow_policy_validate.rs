//! Validate project flow policy against catalog vocabulary.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::Serialize;

use crate::flow_policy_vocabulary::{CapabilityVocabEntry, ProjectFlowVocabulary};
use crate::plan_flow_policy::{FlowPolicy, OperatorDisposition};

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
    let label_dims = label_dimensions(vocab);
    let sink_catalogs = sink_to_catalogs(vocab);
    let all_sinks: HashSet<&str> = sink_catalogs.keys().copied().collect();
    let sink_dims = sink_dimensions(vocab);
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
    let capabilities = capability_index(vocab);

    let _ = policy.default_posture; // presence via serde Default; publish-time required elsewhere

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
            } else if let (Some(ld), Some(sd)) = (label_dims.get(label), sink_dims.get(sink_s)) {
                if ld != sd {
                    warnings.push(FlowPolicyDiagnostic {
                        severity: "warning".into(),
                        code: "dimension_mismatch".into(),
                        message: format!(
                            "Label `{label}` is {ld} but sink `{sink_s}` drains {sd} — confidentiality and integrity lattices should not mix."
                        ),
                        rule_index: Some(idx),
                        field: Some("to_sink".into()),
                        token: Some(sink_s.to_string()),
                        json_pointer: Some(format!("/forbidden/{idx}")),
                    });
                }
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

    for (idx, rule) in policy.capability_gates.iter().enumerate() {
        let cap = rule.pattern.capability.trim();
        if cap.is_empty() {
            errors.push(FlowPolicyDiagnostic {
                severity: "error".into(),
                code: "missing_capability".into(),
                message: "Capability gates require a non-empty pattern.capability.".into(),
                rule_index: Some(idx),
                field: Some("pattern.capability".into()),
                token: None,
                json_pointer: Some(format!("/capability_gates/{idx}/pattern/capability")),
            });
        } else if !capabilities.is_empty() && !capability_known(&capabilities, &rule.pattern) {
            errors.push(FlowPolicyDiagnostic {
                severity: "error".into(),
                code: "unknown_capability".into(),
                message: format!(
                    "Capability `{cap}` is not present in enabled catalog vocabulary."
                ),
                rule_index: Some(idx),
                field: Some("pattern.capability".into()),
                token: Some(cap.to_string()),
                json_pointer: Some(format!("/capability_gates/{idx}/pattern/capability")),
            });
        }
        if let Some(entry_id) = rule.pattern.entry_id.as_ref() {
            if !enabled_entries.contains(entry_id.as_str()) {
                errors.push(FlowPolicyDiagnostic {
                    severity: "error".into(),
                    code: "unknown_entry_id".into(),
                    message: format!(
                        "Capability gate references catalog `{entry_id}` which is not enabled on any MCP bundle."
                    ),
                    rule_index: Some(idx),
                    field: Some("pattern.entry_id".into()),
                    token: Some(entry_id.clone()),
                    json_pointer: Some(format!("/capability_gates/{idx}/pattern/entry_id")),
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
                        json_pointer: Some(format!("/capability_gates/{idx}/pattern/entity")),
                    });
                }
            }
        }
        if rule.pattern.entry_id.is_none()
            && rule.pattern.entity.is_none()
            && matches!(
                rule.enforcement,
                OperatorDisposition::Deny | OperatorDisposition::Approve
            )
        {
            warnings.push(FlowPolicyDiagnostic {
                severity: "warning".into(),
                code: "broad_capability_gate".into(),
                message: "Broad capability gate matches all catalogs — narrower rules below may never apply (first match wins).".into(),
                rule_index: Some(idx),
                field: None,
                token: None,
                json_pointer: Some(format!("/capability_gates/{idx}")),
            });
        }
    }

    for (idx, san) in policy.sanitizers.iter().enumerate() {
        let cap = san.capability.trim();
        if cap.is_empty() {
            errors.push(FlowPolicyDiagnostic {
                severity: "error".into(),
                code: "missing_capability".into(),
                message: "Sanitizer recognition requires a capability name.".into(),
                rule_index: Some(idx),
                field: Some("capability".into()),
                token: None,
                json_pointer: Some(format!("/sanitizers/{idx}/capability")),
            });
        } else if let Some(entry) = find_capability(&capabilities, None, None, cap) {
            if !entry.deterministic {
                errors.push(FlowPolicyDiagnostic {
                    severity: "error".into(),
                    code: "non_deterministic_sanitizer".into(),
                    message: format!(
                        "Capability `{cap}` is not a deterministic transform — only deterministic sanitizers may clear labels."
                    ),
                    rule_index: Some(idx),
                    field: Some("capability".into()),
                    token: Some(cap.to_string()),
                    json_pointer: Some(format!("/sanitizers/{idx}/capability")),
                });
            }
        }
        for label in &san.clears {
            let l = label.as_str();
            if !all_labels.contains(l) {
                errors.push(FlowPolicyDiagnostic {
                    severity: "error".into(),
                    code: "unknown_sanitizer_label".into(),
                    message: format!("Sanitizer clears unknown label `{l}`."),
                    rule_index: Some(idx),
                    field: Some("clears".into()),
                    token: Some(l.to_string()),
                    json_pointer: Some(format!("/sanitizers/{idx}/clears")),
                });
            }
        }
    }

    detect_duplicate_forbidden(policy, &mut warnings);

    // Warn if default_deny with zero allow/approve gates (likely lockout).
    if matches!(policy.default_posture, OperatorDisposition::Deny)
        && !policy.capability_gates.iter().any(|g| {
            matches!(
                g.enforcement,
                OperatorDisposition::Allow | OperatorDisposition::Approve
            )
        })
    {
        warnings.push(FlowPolicyDiagnostic {
            severity: "warning".into(),
            code: "deny_posture_no_allow_gates".into(),
            message: "default_posture is deny but no capability_gates allow or approve — all mutations will be blocked.".into(),
            rule_index: None,
            field: Some("default_posture".into()),
            token: None,
            json_pointer: Some("/default_posture".into()),
        });
    }

    // Warn if default_approve with zero allow gates — every unmatched mutation needs HITL.
    if matches!(policy.default_posture, OperatorDisposition::Approve)
        && !policy
            .capability_gates
            .iter()
            .any(|g| matches!(g.enforcement, OperatorDisposition::Allow))
    {
        warnings.push(FlowPolicyDiagnostic {
            severity: "warning".into(),
            code: "approve_posture_no_allow_gates".into(),
            message: "default_posture is approve but no capability_gates allow — every unmatched mutation will require HITL.".into(),
            rule_index: None,
            field: Some("default_posture".into()),
            token: None,
            json_pointer: Some("/default_posture".into()),
        });
    }

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

fn label_dimensions(vocab: &ProjectFlowVocabulary) -> HashMap<&str, &str> {
    let mut out = HashMap::new();
    for cat in &vocab.catalogs {
        for dc in &cat.data_classes {
            out.entry(dc.id.as_str()).or_insert(dc.dimension.as_str());
        }
    }
    out
}

fn sink_to_catalogs(vocab: &ProjectFlowVocabulary) -> HashMap<&str, Vec<&str>> {
    let mut out: HashMap<&str, Vec<&str>> = HashMap::new();
    for cat in &vocab.catalogs {
        for sink in &cat.sink_classes {
            out.entry(sink.id.as_str())
                .or_default()
                .push(cat.entry_id.as_str());
        }
    }
    out
}

fn sink_dimensions(vocab: &ProjectFlowVocabulary) -> HashMap<&str, &str> {
    let mut out = HashMap::new();
    for cat in &vocab.catalogs {
        for sink in &cat.sink_classes {
            out.entry(sink.id.as_str())
                .or_insert(sink.dimension.as_str());
        }
    }
    out
}

fn capability_index(vocab: &ProjectFlowVocabulary) -> Vec<(String, CapabilityVocabEntry)> {
    let mut out = Vec::new();
    for cat in &vocab.catalogs {
        for cap in &cat.capabilities {
            out.push((cat.entry_id.clone(), cap.clone()));
        }
    }
    out
}

fn capability_known(
    caps: &[(String, CapabilityVocabEntry)],
    pattern: &crate::plan_flow_policy::CapabilityGatePattern,
) -> bool {
    find_capability(
        caps,
        pattern.entry_id.as_deref(),
        pattern.entity.as_deref(),
        pattern.capability.as_str(),
    )
    .is_some()
}

fn find_capability<'a>(
    caps: &'a [(String, CapabilityVocabEntry)],
    entry_id: Option<&str>,
    entity: Option<&str>,
    name: &str,
) -> Option<&'a CapabilityVocabEntry> {
    caps.iter()
        .find(|(eid, cap)| {
            entry_id.is_none_or(|want| want == eid.as_str())
                && entity.is_none_or(|want| want == cap.entity.as_str())
                && cap.name == name
        })
        .map(|(_, cap)| cap)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_policy_vocabulary::{
        CapabilityVocabEntry, CatalogVocabulary, DataClassVocabEntry, SinkClassVocabEntry,
    };
    use crate::plan_flow_policy::{FlowPolicy, ForbiddenFlowRule};
    use plasm_core::{DataClassName, SinkClassName};

    fn sample_vocab() -> ProjectFlowVocabulary {
        ProjectFlowVocabulary {
            catalogs: vec![CatalogVocabulary {
                entry_id: "vultr".into(),
                catalog_has_labels: true,
                data_classes: vec![
                    DataClassVocabEntry {
                        id: "credentials".into(),
                        severity: "critical".into(),
                        dimension: "confidentiality".into(),
                        description: None,
                    },
                    DataClassVocabEntry {
                        id: "untrusted".into(),
                        severity: "untrusted".into(),
                        dimension: "integrity".into(),
                        description: None,
                    },
                ],
                sink_classes: vec![
                    SinkClassVocabEntry {
                        id: "external_publish".into(),
                        dimension: "confidentiality".into(),
                    },
                    SinkClassVocabEntry {
                        id: "permission_grant".into(),
                        dimension: "integrity".into(),
                    },
                ],
                entities: vec!["Instance".into()],
                capabilities: vec![CapabilityVocabEntry {
                    entity: "Instance".into(),
                    name: "delete".into(),
                    kind: "delete".into(),
                    effect_class: "write".into(),
                    deterministic: true,
                    output_labels: vec![],
                    sink_classes: vec![],
                    control_params: vec![],
                }],
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
            ..FlowPolicy::empty_allow()
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
            ..FlowPolicy::empty_allow()
        };
        let r = validate_flow_policy(&policy, &sample_vocab());
        assert!(r.ok, "{r:?}");
    }

    #[test]
    fn warns_approve_posture_with_no_allow_gates() {
        let policy = FlowPolicy {
            default_posture: OperatorDisposition::Approve,
            ..FlowPolicy::empty_allow()
        };
        let r = validate_flow_policy(&policy, &sample_vocab());
        assert!(r.ok, "{r:?}");
        assert!(
            r.warnings
                .iter()
                .any(|w| w.code == "approve_posture_no_allow_gates"),
            "{r:?}"
        );
    }

    #[test]
    fn warns_dimension_mismatch() {
        let policy = FlowPolicy {
            forbidden: vec![ForbiddenFlowRule {
                from_label: DataClassName::new("credentials").unwrap(),
                to_sink: Some(SinkClassName::new("permission_grant").unwrap()),
                reason: Some("cross lattice".into()),
            }],
            ..FlowPolicy::empty_allow()
        };
        let r = validate_flow_policy(&policy, &sample_vocab());
        assert!(r.ok);
        assert!(r.warnings.iter().any(|e| e.code == "dimension_mismatch"));
    }
}
