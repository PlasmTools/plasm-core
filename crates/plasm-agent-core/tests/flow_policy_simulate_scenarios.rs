//! Normative Vultr–Linear Simulate scenario pack goldens (S1–S8).
//!
//! Pack: `fixtures/flow-policies/vultr-linear-simulate-scenarios.json`
//! Policy: `fixtures/flow-policies/vultr-linear-ops-security.json`
//!
//! Full `simulate_flow_policy` needs a live catalog host; these goldens lock the
//! pack schema, seed⊆CGS entity names, and capability-gate dispositions that
//! drive S3/S5/S6 expected dry verdicts under the preset.

use std::fs;
use std::path::{Path, PathBuf};

use plasm_agent_core::{
    EffectEvent, FlowPolicy, NodeDisposition, OperatorDisposition,
};
use plasm_agent_core::plasm_plan::{EffectClass, PlanNodeKind};
use serde::Deserialize;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("workspace root")
}

fn pack_path() -> PathBuf {
    workspace_root().join("fixtures/flow-policies/vultr-linear-simulate-scenarios.json")
}

fn policy_path() -> PathBuf {
    workspace_root().join("fixtures/flow-policies/vultr-linear-ops-security.json")
}

#[derive(Debug, Deserialize)]
struct ScenarioPack {
    version: u32,
    policy_fixture: String,
    scenarios: Vec<Scenario>,
}

#[derive(Debug, Deserialize)]
struct Scenario {
    id: String,
    label: String,
    seeds: Vec<Seed>,
    program: String,
    expected_dry_verdict: String,
    #[serde(default)]
    expected_approval_gates: bool,
}

#[derive(Debug, Deserialize)]
struct Seed {
    api: String,
    entity: String,
}

fn load_pack() -> ScenarioPack {
    let body = fs::read_to_string(pack_path()).expect("read scenario pack");
    serde_json::from_str(&body).expect("parse scenario pack")
}

fn load_preset_policy() -> FlowPolicy {
    let body = fs::read_to_string(policy_path()).expect("read policy fixture");
    serde_json::from_str(&body).expect("parse policy fixture")
}

fn entity_declared(domain_yaml: &str, entity: &str) -> bool {
    let header = format!("  {entity}:");
    domain_yaml.lines().any(|l| l.trim_end() == header)
        || domain_yaml.contains(&format!("entity: {entity}"))
}

#[test]
fn simulate_pack_has_eight_normative_scenarios() {
    let pack = load_pack();
    assert_eq!(pack.version, 1);
    assert_eq!(pack.scenarios.len(), 8);
    assert!(pack.policy_fixture.contains("vultr-linear-ops-security.json"));
    let ids: Vec<_> = pack.scenarios.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8"]);
    for sc in &pack.scenarios {
        assert!(!sc.label.is_empty(), "{}: empty label", sc.id);
        assert!(!sc.program.trim().is_empty(), "{}: empty program", sc.id);
        assert!(
            matches!(sc.expected_dry_verdict.as_str(), "ok" | "review" | "deny"),
            "{}: bad verdict {}",
            sc.id,
            sc.expected_dry_verdict
        );
        assert!(!sc.seeds.is_empty(), "{}: empty seeds", sc.id);
    }
}

#[test]
fn simulate_pack_seeds_exist_in_catalog_domains() {
    let pack = load_pack();
    let root = workspace_root();
    let mut errors = Vec::new();
    for sc in &pack.scenarios {
        for seed in &sc.seeds {
            let path = root.join(format!("apis/{}/domain.yaml", seed.api));
            let Ok(body) = fs::read_to_string(&path) else {
                errors.push(format!("{}: missing {}", sc.id, path.display()));
                continue;
            };
            if !entity_declared(&body, &seed.entity) {
                errors.push(format!(
                    "{}: entity {}/{} not in {}",
                    sc.id,
                    seed.api,
                    seed.entity,
                    path.display()
                ));
            }
        }
    }
    assert!(errors.is_empty(), "seed validation failed:\n{}", errors.join("\n"));
}

#[test]
fn s3_cluster_delete_gate_is_deny() {
    let policy = load_preset_policy();
    let event = EffectEvent {
        entry_id: "vultr".into(),
        entity: "KubernetesCluster".into(),
        kind: PlanNodeKind::Delete,
        effect_class: EffectClass::Write,
        capability: "delete_with_linked_resources".into(),
    };
    assert!(
        matches!(
            policy.disposition_for_event(&event, None),
            NodeDisposition::Deny
        ),
        "S3 expected deny gate"
    );
    let pack = load_pack();
    let s3 = pack.scenarios.iter().find(|s| s.id == "s3").unwrap();
    assert_eq!(s3.expected_dry_verdict, "deny");
}

#[test]
fn s5_instance_delete_gate_is_approve() {
    let policy = load_preset_policy();
    let event = EffectEvent {
        entry_id: "vultr".into(),
        entity: "Instance".into(),
        kind: PlanNodeKind::Delete,
        effect_class: EffectClass::Write,
        capability: "delete".into(),
    };
    assert!(
        matches!(
            policy.disposition_for_event(&event, None),
            NodeDisposition::Approve { .. }
        ),
        "S5 expected approve gate"
    );
    let pack = load_pack();
    let s5 = pack.scenarios.iter().find(|s| s.id == "s5").unwrap();
    assert_eq!(s5.expected_dry_verdict, "ok");
    assert!(s5.expected_approval_gates);
}

#[test]
fn s6_firewall_rule_create_gate_is_approve() {
    let policy = load_preset_policy();
    let event = EffectEvent {
        entry_id: "vultr".into(),
        entity: "FirewallRule".into(),
        kind: PlanNodeKind::Create,
        effect_class: EffectClass::Write,
        capability: "create".into(),
    };
    assert!(
        matches!(
            policy.disposition_for_event(&event, None),
            NodeDisposition::Approve { .. }
        ),
        "S6 expected approve gate"
    );
    let pack = load_pack();
    let s6 = pack.scenarios.iter().find(|s| s.id == "s6").unwrap();
    assert_eq!(s6.expected_dry_verdict, "ok");
    assert!(s6.expected_approval_gates);
    assert!(
        s6.program.contains("firewall_group_id"),
        "S6 must use catalog wire firewall_group_id"
    );
    assert!(
        !s6.program.contains("Firewall("),
        "S6 must not use nonexistent Firewall entity"
    );
}

#[test]
fn s1_s2_forbidden_rules_present_in_preset() {
    let policy = load_preset_policy();
    let labels: Vec<_> = policy
        .forbidden
        .iter()
        .map(|r| (r.from_label.as_str(), r.to_sink.as_deref()))
        .collect();
    assert!(
        labels
            .iter()
            .any(|(l, s)| *l == "credentials" && *s == Some("external_publish")),
        "S1 needs credentials→external_publish"
    );
    assert!(
        labels
            .iter()
            .any(|(l, s)| *l == "untrusted" && *s == Some("permission_grant")),
        "S2 needs untrusted→permission_grant"
    );
}

#[test]
fn preset_gates_cover_s3_s5_s6_patterns() {
    let policy = load_preset_policy();
    let patterns: Vec<_> = policy
        .capability_gates
        .iter()
        .map(|g| {
            (
                g.pattern.entry_id.as_deref(),
                g.pattern.entity.as_deref(),
                g.pattern.capability.as_str(),
                g.enforcement,
            )
        })
        .collect();

    assert!(patterns.iter().any(|(e, ent, cap, enf)| {
        *e == Some("vultr")
            && *ent == Some("KubernetesCluster")
            && *cap == "delete_with_linked_resources"
            && *enf == OperatorDisposition::Deny
    }));
    assert!(patterns.iter().any(|(e, ent, cap, enf)| {
        *e == Some("vultr")
            && *ent == Some("Instance")
            && *cap == "delete"
            && *enf == OperatorDisposition::Approve
    }));
    assert!(patterns.iter().any(|(e, ent, cap, enf)| {
        *e == Some("vultr")
            && *ent == Some("FirewallRule")
            && *cap == "create"
            && *enf == OperatorDisposition::Approve
    }));
}

/// Hermit twin: real `simulate_flow_policy_with_options` dry_verdict against matrix CGS.
mod live_dry_run {
    use super::*;
    use std::sync::Arc;

    use plasm_agent_core::flow_policy_repository::FlowPolicyRow;
    use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
    use plasm_agent_core::http_execute::CapabilitySeed;
    use plasm_agent_core::{
        simulate_flow_policy_with_options, CapabilityGatePattern, CapabilityGateRule,
        SimulateOptions, SimulatePolicyArm,
    };
    use plasm_agent_core::server_state::CatalogBootstrap;
    use plasm_core::discovery::InMemoryCgsRegistry;
    use plasm_core::loader::load_schema_dir;
    use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

    fn empty_row() -> FlowPolicyRow {
        FlowPolicyRow {
            tenant_id: "t".into(),
            workspace_slug: "w".into(),
            project_slug: "p".into(),
            published_revision: 0,
            published_policy: None,
            published_at: None,
            published_by_subject: None,
            draft_policy: None,
            draft_updated_at: None,
            draft_validated_at: None,
            draft_validation_ok: None,
        }
    }

    fn matrix_host() -> plasm_agent_core::server_state::PlasmHostState {
        if std::env::var_os("PLASM_HTTP_NO_SYSTEM_PROXY").is_none() {
            unsafe { std::env::set_var("PLASM_HTTP_NO_SYSTEM_PROXY", "1") };
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = Arc::new(load_schema_dir(&dir).expect("plasm_language_matrix"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "langmatrix".into(),
            "Lang Matrix".into(),
            vec!["matrix".into()],
            cgs,
        )]);
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(reg),
            catalog_bootstrap: CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(plasm_agent_core::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        })
    }

    #[tokio::test]
    async fn ephemeral_deny_gate_returns_deny_dry_verdict() {
        let st = matrix_host();
        let policy = FlowPolicy {
            capability_gates: vec![CapabilityGateRule {
                pattern: CapabilityGatePattern {
                    entry_id: None,
                    entity: Some("LangItem".into()),
                    capability: "delete".into(),
                },
                enforcement: OperatorDisposition::Deny,
            }],
            ..FlowPolicy::empty_allow()
        };

        let result = simulate_flow_policy_with_options(
            &st,
            &empty_row(),
            SimulatePolicyArm::Draft,
            vec![CapabilitySeed {
                entry_id: "langmatrix".into(),
                entity: "LangItem".into(),
            }],
            r#"LangItem("i2").delete()"#,
            "matrix deny golden",
            SimulateOptions {
                ephemeral_policy: Some(policy),
            },
        )
        .await
        .expect("simulate");

        assert_eq!(result.dry_verdict, "deny");
    }

    #[tokio::test]
    async fn ephemeral_empty_allow_read_returns_ok() {
        let st = matrix_host();
        let result = simulate_flow_policy_with_options(
            &st,
            &empty_row(),
            SimulatePolicyArm::Draft,
            vec![CapabilitySeed {
                entry_id: "langmatrix".into(),
                entity: "LangItem".into(),
            }],
            r#"LangItem("i2")"#,
            "matrix ok golden",
            SimulateOptions {
                ephemeral_policy: Some(FlowPolicy::empty_allow()),
            },
        )
        .await
        .expect("simulate");

        assert_eq!(result.dry_verdict, "ok");
    }
}

