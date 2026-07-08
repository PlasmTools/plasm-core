//! Dev/demo flow policy loading from environment (`PLASM_FLOW_POLICY_PATH`).

use crate::plan_flow_policy::{FlowPolicy, FlowPolicySnapshot, PolicyRevision};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::warn;

static ENV_POLICY: OnceLock<FlowPolicySnapshot> = OnceLock::new();
static ENV_POLICY_WARNED: OnceLock<()> = OnceLock::new();

fn warn_once(msg: impl AsRef<str>) {
    if ENV_POLICY_WARNED.set(()).is_ok() {
        warn!("{}", msg.as_ref());
    }
}

fn resolve_policy_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PLASM_FLOW_POLICY_PATH") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    match std::env::var("PLASM_FLOW_POLICY_PRESET").ok().as_deref() {
        Some("vultr-linear-ops") | Some("vultr-linear-ops-security") => Some(PathBuf::from(
            "fixtures/flow-policies/vultr-linear-ops-security.json",
        )),
        Some(other) => {
            warn_once(format!(
                "PLASM_FLOW_POLICY_PRESET={other:?} is unknown; ignoring preset"
            ));
            None
        }
        None => None,
    }
}

fn load_policy_from_path(path: &Path) -> Option<FlowPolicy> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn snapshot_from_path(path: &Path) -> Option<FlowPolicySnapshot> {
    let policy = load_policy_from_path(path)?;
    Some(FlowPolicySnapshot::Active {
        revision: PolicyRevision(1),
        policy,
    })
}

/// Session flow policy pinned from env when configured; otherwise inactive default.
pub fn flow_policy_from_env_or_default() -> FlowPolicySnapshot {
    ENV_POLICY
        .get_or_init(|| {
            let Some(path) = resolve_policy_path() else {
                return FlowPolicySnapshot::inactive_default();
            };
            match snapshot_from_path(&path) {
                Some(snapshot) => snapshot,
                None => {
                    warn_once(format!(
                        "PLASM_FLOW_POLICY_PATH/PRESET could not load policy from {}; using inactive default",
                        path.display()
                    ));
                    FlowPolicySnapshot::inactive_default()
                }
            }
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_flow_policy::{
        CapabilityGatePattern, CapabilityGateRule, ForbiddenFlowRule, OperatorDisposition,
    };
    use plasm_core::{DataClassName, SinkClassName};
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn loads_forbidden_rules_from_json_file() {
        let policy = FlowPolicy {
            forbidden: vec![ForbiddenFlowRule {
                from_label: DataClassName::new("credentials").expect("label"),
                to_sink: Some(SinkClassName::new("external_publish").expect("sink")),
                reason: Some("demo".into()),
            }],
            capability_gates: vec![CapabilityGateRule {
                pattern: CapabilityGatePattern {
                    entry_id: Some("vultr".into()),
                    entity: Some("KubernetesCluster".into()),
                    capability: "delete".into(),
                },
                enforcement: OperatorDisposition::Deny,
            }],
            sanitizers: Vec::new(),
            ..FlowPolicy::default()
        };
        let mut file = NamedTempFile::new().expect("temp");
        write!(file, "{}", serde_json::to_string(&policy).expect("json")).expect("write");
        let snapshot = snapshot_from_path(file.path()).expect("load");
        assert!(matches!(snapshot, FlowPolicySnapshot::Active { .. }));
        if let FlowPolicySnapshot::Active { policy, .. } = snapshot {
            assert_eq!(policy.forbidden.len(), 1);
            assert_eq!(policy.capability_gates.len(), 1);
        }
    }

    #[test]
    fn bundled_vultr_linear_ops_security_preset_parses() {
        let path = PathBuf::from("fixtures/flow-policies/vultr-linear-ops-security.json");
        if !path.exists() {
            return;
        }
        let snapshot = snapshot_from_path(&path).expect("bundled preset");
        let FlowPolicySnapshot::Active { policy, .. } = snapshot else {
            panic!("expected active snapshot");
        };
        assert_eq!(policy.forbidden.len(), 3);
        assert!(policy.capability_gates.len() >= 5);
    }
}
