//! The agent presentation seam. Machine evidence is input, never serializable output.

pub(crate) fn source_binding_label(label: &str) -> Option<&str> {
    (!label.is_empty() && !crate::plan_dry_display::is_synthetic_plan_node_id_public(label))
        .then_some(label)
}

/// Agent-facing semantic text. Intentionally has no Serialize implementation.
#[derive(Debug, Clone)]
pub struct AgentMessage(String);
impl AgentMessage {
    pub fn into_string(self) -> String {
        self.0
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::ops::Deref for AgentMessage {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for AgentMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Only program-owned path segments may enter a correction location.
struct AgentNodePath(String);
impl AgentNodePath {
    fn from_execution_path(path: &str) -> Self {
        Self(
            path.split('/')
                .filter(|part| source_binding_label(part).is_some())
                .collect::<Vec<_>>()
                .join("/"),
        )
    }
}
impl std::fmt::Display for AgentNodePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn render_failure(failure: &plasm_runtime::ExecutionFailure) -> AgentMessage {
    use plasm_runtime::{MutationDispatchStatus, RecoveryDisposition};
    let visible_node = failure
        .node
        .as_deref()
        .map(AgentNodePath::from_execution_path);
    let visible_node = visible_node.as_ref().filter(|path| !path.0.is_empty());
    let mut lines = vec![match visible_node {
        Some(node) => format!("Execution stopped at {node}: {}", failure.diagnostic()),
        None => format!("Execution stopped: {}", failure.diagnostic()),
    }];
    let mut effects = Vec::new();
    for receipt in &failure.effects {
        let identities = |status: plasm_runtime::OperationInvocationStatus| {
            let matching = receipt
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.status == status);
            let count = matching.clone().count();
            let mut visible = matching
                .take(4)
                .map(|occurrence| {
                    let path = occurrence
                        .occurrence_path
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join("/");
                    let scope =
                        AgentNodePath::from_execution_path(&occurrence.scope_path.join("/"));
                    let coordinate = format!(
                        "occurrence [{path}]{}",
                        if scope.0.is_empty() {
                            String::new()
                        } else {
                            format!(" at {scope}")
                        }
                    );
                    match &occurrence.source_identity {
                        Some(identity) if identity.chars().count() <= 96 => {
                            format!("{identity} ({coordinate})")
                        }
                        Some(_) => format!(
                            "{coordinate} (identity exceeds inline limit; see execution evidence)"
                        ),
                        None => coordinate,
                    }
                })
                .collect::<Vec<_>>();
            if count > visible.len() {
                visible.push(format!(
                    "{} more; full identities in execution evidence",
                    count - visible.len()
                ));
            }
            visible
        };
        if receipt.completed > 0 {
            let completed = identities(plasm_runtime::OperationInvocationStatus::Completed);
            effects.push(format!(
                "{}/{}: {} completed{}",
                receipt.entry_id,
                receipt.capability,
                receipt.completed,
                if completed.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", completed.join(", "))
                }
            ));
        }
        if receipt.failed > 0 {
            let failed = identities(plasm_runtime::OperationInvocationStatus::Failed);
            effects.push(format!(
                "{}/{}: {} failed{}",
                receipt.entry_id,
                receipt.capability,
                receipt.failed,
                if failed.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", failed.join(", "))
                }
            ));
        }
    }
    let has_dispatch_evidence = !failure.dispatches.is_empty();
    let mut dispatches = std::collections::BTreeMap::new();
    for dispatch in &failure.dispatches {
        let status = match dispatch.status {
            MutationDispatchStatus::Unresolved => "without transport response",
            MutationDispatchStatus::ResponseReceived => "with transport response",
        };
        *dispatches
            .entry((
                dispatch.operation.entry_id.as_str(),
                dispatch.operation.capability.as_str(),
                status,
            ))
            .or_insert(0usize) += 1;
    }
    for ((entry, capability, status), count) in dispatches {
        effects.push(format!("{entry}/{capability}: {count} {status}"));
    }
    if !effects.is_empty() {
        lines.push(format!("Effects: {}.", effects.join("; ")));
        if has_dispatch_evidence {
            lines.push("Dispatch history may overlap completed and failed effects above.".into());
        }
    }
    if failure.effects_unresolved && !has_dispatch_evidence {
        lines.push("Some effects remain unresolved.".into());
    }
    lines.push(
        match failure.recovery {
            RecoveryDisposition::RepairProgram => "Revise the program and plan again.",
            RecoveryDisposition::ObtainAuthorization => "Obtain authorization before another run.",
            RecoveryDisposition::ReconcileEffects => {
                "Check completed and unresolved effects before another run; this run did not roll back."
            }
            RecoveryDisposition::Stop => "Stop this execution.",
        }
        .into(),
    );
    AgentMessage(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn nested_effects_keep_distinct_coordinates_when_local_indexes_repeat() {
        let mut failure = plasm_runtime::ExecutionFailure::new(
            plasm_runtime::FailureCause::Upstream,
            "rejected",
            "Rejected",
        );
        failure
            .effects
            .push(plasm_runtime::execution_failure::EffectReceipt {
                resolved_effects: Vec::new(),
                entry_id: "fixture".into(),
                capability: "write".into(),
                completed: 2,
                failed: 0,
                occurrences: (0..2)
                    .map(|outer| plasm_runtime::execution_failure::EffectOccurrence {
                        scope_path: vec!["batch".into(), "__py12".into()],
                        occurrence_path: vec![outer, 0],
                        source_index: 0,
                        source_identity: None,
                        status: plasm_runtime::OperationInvocationStatus::Completed,
                    })
                    .collect(),
            });
        let text = render_failure(&failure);
        assert!(text.contains("occurrence [0/0] at batch"), "{text}");
        assert!(text.contains("occurrence [1/0] at batch"), "{text}");
        assert!(!text.contains("__py12"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        #[test]
        fn effect_identity_previews_are_bounded_without_altering_evidence(count in 5usize..1024, identity in ".{0,300}") {
            let mut failure = plasm_runtime::ExecutionFailure::new(plasm_runtime::FailureCause::Upstream, "rejected", "Request rejected");
            failure.effects.push(plasm_runtime::execution_failure::EffectReceipt {
                resolved_effects: Vec::new(),
                entry_id:"matrix".into(), capability:"write".into(), completed:count, failed:0,
                occurrences:(0..count).map(|source_index|plasm_runtime::execution_failure::EffectOccurrence {
                    scope_path: vec!["write".into()], occurrence_path:vec![source_index],
                    source_index, source_identity:Some(identity.clone()), status:plasm_runtime::OperationInvocationStatus::Completed,
                }).collect(),
            });
            let before = serde_json::to_value(&failure).unwrap();
            let text = render_failure(&failure);
            prop_assert!(text.len() < 2400);
            let count_text = format!("{} completed", count);
            prop_assert!(text.contains(&count_text));
            prop_assert!(text.contains("full identities in execution evidence"));
            prop_assert_eq!(serde_json::to_value(&failure).unwrap(), before);
        }
    }
}
