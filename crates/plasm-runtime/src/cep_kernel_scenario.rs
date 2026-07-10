//! CEP MVCC kernel scenario types for Quint JSON fixture replay (see `tests/cep_kernel_from_quint.rs`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use plasm_core::{Ref, TypedFieldValue, Value};

use crate::branch_commit::{detect_materialization_conflicts, BranchMaterializationBase};
use crate::{CachedEntity, EntityCompleteness, SessionMaterialization};
use serde::{Deserialize, Serialize};

const KERNEL_ENTITY_TYPE: &str = "Berry";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CepKernelScenario {
    pub name: String,
    #[serde(default)]
    pub cep_ids: Vec<String>,
    #[serde(default)]
    pub quint_test: Option<String>,
    pub ops: Vec<CepKernelOp>,
    #[serde(default)]
    pub assert_session_fields: Vec<SessionFieldAssert>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionFieldAssert {
    #[serde(rename = "ref")]
    pub ref_id: u32,
    pub field: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CepKernelOp {
    SessionUpsertField {
        #[serde(rename = "ref")]
        ref_id: u32,
        field: i32,
    },
    SessionUpsertRel {
        #[serde(rename = "ref")]
        ref_id: u32,
        rel: Vec<u32>,
    },
    ForkBranch {
        branch_id: u32,
    },
    BranchUpsertField {
        branch_id: u32,
        #[serde(rename = "ref")]
        ref_id: u32,
        field: i32,
    },
    BranchUpsertRel {
        branch_id: u32,
        #[serde(rename = "ref")]
        ref_id: u32,
        rel: Vec<u32>,
    },
    TryCommit {
        branch_id: u32,
        expect: CommitExpect,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CommitExpect {
    Success,
    WriteConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    UnknownBranch(u32),
    BranchIdAlreadyLive(u32),
    CommitExpectationMismatch {
        branch_id: u32,
        expected: CommitExpect,
        got: CommitExpect,
    },
    SessionFieldMismatch {
        ref_id: u32,
        expected: i32,
        got: Option<i32>,
    },
}

struct LiveBranch {
    mat: SessionMaterialization,
    base: BranchMaterializationBase,
}

pub fn kernel_ref(ref_id: u32) -> Ref {
    Ref::new(KERNEL_ENTITY_TYPE, ref_id.to_string())
}

pub fn kernel_entity(ref_id: u32, field: i32, rel: &[u32]) -> CachedEntity {
    let reference = kernel_ref(ref_id);
    let mut fields = IndexMap::new();
    fields.insert(
        "field".into(),
        TypedFieldValue::from(Value::Integer(field as i64)),
    );
    let mut relations = IndexMap::new();
    if !rel.is_empty() {
        relations.insert(
            "rel".into(),
            rel.iter().map(|id| kernel_ref(*id)).collect::<Vec<_>>(),
        );
    }
    CachedEntity {
        reference,
        fields,
        relations,
        last_updated: 1,
        version: 1,
        completeness: EntityCompleteness::Summary,
    }
}

pub fn session_scalar_field(session: &SessionMaterialization, ref_id: u32) -> Option<i32> {
    session.get(&kernel_ref(ref_id)).and_then(|e| {
        e.get_field("field").and_then(|v| match v.to_value() {
            Value::Integer(n) => i32::try_from(n).ok(),
            _ => None,
        })
    })
}

pub fn replay_scenario(scenario: &CepKernelScenario) -> Result<(), ReplayError> {
    let mut session = SessionMaterialization::new();
    let mut branches: HashMap<u32, LiveBranch> = HashMap::new();

    for op in &scenario.ops {
        match op {
            CepKernelOp::SessionUpsertField { ref_id, field } => {
                session
                    .insert(kernel_entity(*ref_id, *field, &[]))
                    .expect("session insert");
            }
            CepKernelOp::SessionUpsertRel { ref_id, rel } => {
                let prior_field = session_scalar_field(&session, *ref_id).unwrap_or(0);
                session
                    .insert(kernel_entity(*ref_id, prior_field, rel))
                    .expect("session rel insert");
            }
            CepKernelOp::ForkBranch { branch_id } => {
                if branches.contains_key(branch_id) {
                    return Err(ReplayError::BranchIdAlreadyLive(*branch_id));
                }
                let (mat, base) = BranchMaterializationBase::fork_from(&session);
                branches.insert(*branch_id, LiveBranch { mat, base });
            }
            CepKernelOp::BranchUpsertField {
                branch_id,
                ref_id,
                field,
            } => {
                let branch = branches
                    .get_mut(branch_id)
                    .ok_or(ReplayError::UnknownBranch(*branch_id))?;
                branch
                    .mat
                    .insert(kernel_entity(*ref_id, *field, &[]))
                    .expect("branch insert");
            }
            CepKernelOp::BranchUpsertRel {
                branch_id,
                ref_id,
                rel,
            } => {
                let branch = branches
                    .get_mut(branch_id)
                    .ok_or(ReplayError::UnknownBranch(*branch_id))?;
                let prior_field = session_scalar_field(&branch.mat, *ref_id).unwrap_or(0);
                branch
                    .mat
                    .insert(kernel_entity(*ref_id, prior_field, rel))
                    .expect("branch rel insert");
            }
            CepKernelOp::TryCommit { branch_id, expect } => {
                let branch = branches
                    .remove(branch_id)
                    .ok_or(ReplayError::UnknownBranch(*branch_id))?;
                let conflicts =
                    detect_materialization_conflicts(&session, &branch.base, &branch.mat);
                let got = if conflicts.has_any() {
                    CommitExpect::WriteConflict
                } else {
                    session.absorb_branch(branch.mat).expect("absorb branch");
                    CommitExpect::Success
                };
                if got != *expect {
                    return Err(ReplayError::CommitExpectationMismatch {
                        branch_id: *branch_id,
                        expected: *expect,
                        got,
                    });
                }
            }
        }
    }

    for assert in &scenario.assert_session_fields {
        let got = session_scalar_field(&session, assert.ref_id);
        if got != Some(assert.field) {
            return Err(ReplayError::SessionFieldMismatch {
                ref_id: assert.ref_id,
                expected: assert.field,
                got,
            });
        }
    }

    Ok(())
}

pub fn scenarios_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../specs/cep-kernel/scenarios")
}

pub fn counterexamples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../specs/cep-kernel/counterexamples")
}

pub fn load_scenario(path: &Path) -> CepKernelScenario {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("read scenario {}: {e}", path.display());
    });
    serde_json::from_str(&raw).unwrap_or_else(|e| {
        panic!("parse scenario {}: {e}", path.display());
    })
}

pub fn load_all_scenarios() -> Vec<CepKernelScenario> {
    let dir = scenarios_dir();
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read scenarios dir {}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    paths.iter().map(|p| load_scenario(p)).collect()
}

/// CEP-2a on the Rust side: after a rejected commit, session graph is unchanged.
pub fn assert_no_stale_absorption(
    session_before: &SessionMaterialization,
    session_after: &SessionMaterialization,
) {
    for reference in session_before.graph.all_references() {
        let before = session_before.get(reference).expect("before ref");
        let after = session_after.get(reference).expect("after ref");
        assert_eq!(
            before.fields, after.fields,
            "stale commit must not mutate session fields on {reference}"
        );
        assert_eq!(
            before.relations, after.relations,
            "stale commit must not mutate session relations on {reference}"
        );
    }
}
