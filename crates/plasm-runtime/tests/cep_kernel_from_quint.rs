//! Replay Quint-exported CEP kernel scenarios against real materialization commit logic.

use plasm_runtime::branch_commit::BranchMaterializationBase;
use plasm_runtime::cep_kernel_scenario::{
    assert_no_stale_absorption, counterexamples_dir, load_all_scenarios, load_scenario,
    replay_scenario, session_scalar_field, CepKernelOp, CepKernelScenario, CommitExpect,
};
use plasm_runtime::SessionMaterialization;
use proptest::prelude::*;

#[test]
fn cep_kernel_from_quint_fixtures_replay() {
    let scenarios = load_all_scenarios();
    assert!(
        scenarios.len() >= 3,
        "expected Quint fixtures under specs/cep-kernel/scenarios/"
    );
    for scenario in &scenarios {
        replay_scenario(scenario).unwrap_or_else(|e| {
            panic!(
                "scenario {} ({:?}) failed: {e:?}",
                scenario.name, scenario.cep_ids
            );
        });
    }
}

#[test]
fn cep_kernel_stale_commit_preserves_session_cep2a() {
    let scenario = load_all_scenarios()
        .into_iter()
        .find(|s| s.name == "stale_commit_rejected")
        .expect("stale_commit_rejected.json fixture");

    let mut session = SessionMaterialization::new();
    let mut branches: std::collections::HashMap<
        u32,
        (SessionMaterialization, BranchMaterializationBase),
    > = std::collections::HashMap::new();

    for op in &scenario.ops {
        match op {
            CepKernelOp::SessionUpsertField { ref_id, field } => {
                session
                    .insert(plasm_runtime::cep_kernel_scenario::kernel_entity(
                        *ref_id,
                        *field,
                        &[],
                    ))
                    .unwrap();
            }
            CepKernelOp::ForkBranch { branch_id } => {
                let (mat, base) = BranchMaterializationBase::fork_from(&session);
                branches.insert(*branch_id, (mat, base));
            }
            CepKernelOp::BranchUpsertField {
                branch_id,
                ref_id,
                field,
            } => {
                let (mat, _) = branches.get_mut(branch_id).expect("branch");
                mat.insert(plasm_runtime::cep_kernel_scenario::kernel_entity(
                    *ref_id,
                    *field,
                    &[],
                ))
                .unwrap();
            }
            CepKernelOp::TryCommit { branch_id, expect } => {
                assert_eq!(*expect, CommitExpect::WriteConflict);
                let (branch_mat, base) = branches.remove(branch_id).expect("branch");
                let before = session.clone();
                let conflicts = plasm_runtime::branch_commit::detect_materialization_conflicts(
                    &session,
                    &base,
                    &branch_mat,
                );
                assert!(conflicts.has_any());
                assert_no_stale_absorption(&before, &session);
            }
            _ => {}
        }
    }
}

#[test]
fn cep_kernel_broken_quint_counterexample_not_reproduced_in_rust() {
    let path = counterexamples_dir().join("violation_no_stale_absorption.json");
    let scenario = load_scenario(&path);

    let mut session = SessionMaterialization::new();
    let mut branches: std::collections::HashMap<
        u32,
        (SessionMaterialization, BranchMaterializationBase),
    > = std::collections::HashMap::new();

    for op in &scenario.ops {
        match op {
            CepKernelOp::SessionUpsertField { ref_id, field } => {
                session
                    .insert(plasm_runtime::cep_kernel_scenario::kernel_entity(
                        *ref_id,
                        *field,
                        &[],
                    ))
                    .unwrap();
            }
            CepKernelOp::SessionUpsertRel { ref_id, rel } => {
                let prior = session_scalar_field(&session, *ref_id).unwrap_or(0);
                session
                    .insert(plasm_runtime::cep_kernel_scenario::kernel_entity(
                        *ref_id, prior, rel,
                    ))
                    .unwrap();
            }
            CepKernelOp::ForkBranch { branch_id } => {
                let (mat, base) = BranchMaterializationBase::fork_from(&session);
                branches.insert(*branch_id, (mat, base));
            }
            CepKernelOp::BranchUpsertField {
                branch_id,
                ref_id,
                field,
            } => {
                let (mat, _) = branches.get_mut(branch_id).expect("branch");
                mat.insert(plasm_runtime::cep_kernel_scenario::kernel_entity(
                    *ref_id,
                    *field,
                    &[],
                ))
                .unwrap();
            }
            CepKernelOp::BranchUpsertRel {
                branch_id,
                ref_id,
                rel,
            } => {
                let (mat, _) = branches.get_mut(branch_id).expect("branch");
                let prior = session_scalar_field(mat, *ref_id).unwrap_or(0);
                mat.insert(plasm_runtime::cep_kernel_scenario::kernel_entity(
                    *ref_id, prior, rel,
                ))
                .unwrap();
            }
            CepKernelOp::TryCommit { branch_id, .. } => {
                let (branch_mat, base) = branches.remove(branch_id).expect("branch");
                let before = session.clone();
                let conflicts = plasm_runtime::branch_commit::detect_materialization_conflicts(
                    &session,
                    &base,
                    &branch_mat,
                );
                if conflicts.has_any() {
                    assert_no_stale_absorption(&before, &session);
                } else {
                    session.absorb_branch(branch_mat).expect("absorb");
                }
            }
        }
    }

    // Fields that only appear in the Quint broken-kernel violation trace (stale absorption).
    for (ref_id, field) in [(2, 2), (3, 1)] {
        let got = session_scalar_field(&session, ref_id);
        assert_ne!(
            got,
            Some(field),
            "Rust must not reproduce Quint broken-kernel absorption (ref {ref_id} field {field})"
        );
    }
}

fn arb_kernel_op() -> impl Strategy<Value = CepKernelOp> {
    prop_oneof![
        (1u32..=3u32, 0i32..=2)
            .prop_map(|(ref_id, field)| CepKernelOp::SessionUpsertField { ref_id, field }),
        Just(CepKernelOp::ForkBranch { branch_id: 1 }),
        (1u32..=2u32, 1u32..=3u32, 0i32..=2).prop_map(|(branch_id, ref_id, field)| {
            CepKernelOp::BranchUpsertField {
                branch_id,
                ref_id,
                field,
            }
        }),
    ]
}

proptest! {
    #[test]
    fn proptest_replay_random_ops_no_panic(ops in prop::collection::vec(arb_kernel_op(), 1..=10)) {
        let scenario = CepKernelScenario {
            name: "proptest".into(),
            cep_ids: vec![],
            quint_test: None,
            ops,
            assert_session_fields: vec![],
        };
        let _ = replay_scenario(&scenario);
    }
}
