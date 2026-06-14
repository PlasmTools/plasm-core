//! Randomized concurrency tests for graph fork/commit and spill rehydrate invariants.
//!
//! Shuttle exercises pure [`SessionMaterialization`] branch ops under PCT scheduling.
//! Tokio tests cover [`GraphExecuteBranch`] + spill I/O (real async mutex / object store).

use std::sync::Arc;

use plasm_core::Ref;
use plasm_runtime::{CachedEntity, SessionMaterialization};
use shuttle::future;
use shuttle::sync::{Arc as ShuttleArc, Mutex as ShuttleMutex};

use crate::graph_execute::GraphCommitError;
use crate::graph_execute::GraphExecuteBranch;
use crate::graph_rehydrate::GraphSurfaceRehydrator;
use crate::test_support::graph_fixtures::{
    berry_entity, load_pokeapi_mini_cgs, spill_one_page, test_execute_session, SpillHostFixture,
};

use super::branch_ops::{commit_materialization, fork_materialization};

const PCT_DEPTH: usize = 4;
const PCT_ITERATIONS: usize = 500;
const RANDOM_ITERATIONS: usize = 2_000;

type ShuttleSession = ShuttleArc<ShuttleMutex<SessionMaterialization>>;

fn shuttle_session() -> ShuttleSession {
    ShuttleArc::new(ShuttleMutex::new(SessionMaterialization::new()))
}

fn lock_session(session: &ShuttleSession) -> shuttle::sync::MutexGuard<'_, SessionMaterialization> {
    session.lock().expect("shuttle mutex")
}

fn insert_on(session: &mut SessionMaterialization, entity: CachedEntity) {
    session.insert(entity).expect("insert");
}

fn has_ref(session: &SessionMaterialization, name: &str) -> bool {
    session.get(&Ref::new("Berry", name)).is_some()
}

#[test]
fn shuttle_branch_commit_merges_entity() {
    shuttle::check_random(
        || {
            future::block_on(async {
                let session = shuttle_session();
                let (mut branch, epoch) = {
                    let guard = lock_session(&session);
                    fork_materialization(&guard)
                };
                insert_on(&mut branch, berry_entity("pecha"));
                {
                    let mut guard = lock_session(&session);
                    commit_materialization(&mut guard, epoch, branch).expect("commit");
                }
                assert!(has_ref(&lock_session(&session), "pecha"));
            });
        },
        RANDOM_ITERATIONS,
    );
}

#[test]
fn shuttle_stale_commit_never_writes_branch_entities() {
    shuttle::check_random(
        || {
            future::block_on(async {
                let session = shuttle_session();
                let (mut branch, epoch) = {
                    let guard = lock_session(&session);
                    fork_materialization(&guard)
                };
                future::yield_now().await;
                insert_on(&mut branch, berry_entity("stale_berry"));
                future::yield_now().await;
                {
                    let mut guard = lock_session(&session);
                    insert_on(&mut guard, berry_entity("session_bump"));
                }
                future::yield_now().await;
                let mut guard = lock_session(&session);
                let err = commit_materialization(&mut guard, epoch, branch).expect_err("stale");
                assert!(matches!(err, GraphCommitError::StaleParentEpoch { .. }));
                assert!(!has_ref(&guard, "stale_berry"));
                assert!(has_ref(&guard, "session_bump"));
            });
        },
        RANDOM_ITERATIONS,
    );
}

#[test]
fn shuttle_read_lock_available_while_branch_alive() {
    shuttle::check_random(
        || {
            future::block_on(async {
                let session = shuttle_session();
                let (mut branch, epoch) = {
                    let guard = lock_session(&session);
                    fork_materialization(&guard)
                };
                future::yield_now().await;
                insert_on(&mut branch, berry_entity("mid"));
                {
                    let _read = lock_session(&session);
                }
                future::yield_now().await;
                let mut guard = lock_session(&session);
                commit_materialization(&mut guard, epoch, branch).expect("commit");
                assert!(has_ref(&guard, "mid"));
            });
        },
        RANDOM_ITERATIONS,
    );
}

/// Two concurrent commits: at most one branch wins; session stays consistent.
#[test]
fn shuttle_dual_fork_epoch_cas() {
    shuttle::check_pct(
        || {
            future::block_on(async {
                let session = shuttle_session();
                let (mut branch_a, epoch_a) = {
                    let guard = lock_session(&session);
                    fork_materialization(&guard)
                };
                future::yield_now().await;
                let (mut branch_b, epoch_b) = {
                    let guard = lock_session(&session);
                    fork_materialization(&guard)
                };
                assert_eq!(epoch_a.0, epoch_b.0);
                insert_on(&mut branch_a, berry_entity("a"));
                insert_on(&mut branch_b, berry_entity("b"));

                let session_a = ShuttleArc::clone(&session);
                let session_b = ShuttleArc::clone(&session);
                future::spawn(async move {
                    future::yield_now().await;
                    let mut guard = lock_session(&session_a);
                    let _ = commit_materialization(&mut guard, epoch_a, branch_a);
                });
                future::spawn(async move {
                    future::yield_now().await;
                    let mut guard = lock_session(&session_b);
                    let _ = commit_materialization(&mut guard, epoch_a, branch_b);
                });
                future::yield_now().await;

                let guard = lock_session(&session);
                let has_a = has_ref(&guard, "a");
                let has_b = has_ref(&guard, "b");
                assert!(
                    !(has_a && has_b),
                    "both branches must not commit after epoch bump from first absorb"
                );
            });
        },
        PCT_DEPTH,
        PCT_ITERATIONS,
    );
}

#[test]
fn shuttle_interleaved_fork_commit() {
    shuttle::check_pct(
        || {
            future::block_on(async {
                let session = shuttle_session();
                let session_b = ShuttleArc::clone(&session);

                future::spawn(async move {
                    let (mut branch, epoch) = {
                        let guard = lock_session(&session);
                        fork_materialization(&guard)
                    };
                    future::yield_now().await;
                    insert_on(&mut branch, berry_entity("interleave"));
                    future::yield_now().await;
                    let mut guard = lock_session(&session);
                    let _ = commit_materialization(&mut guard, epoch, branch);
                });

                future::spawn(async move {
                    future::yield_now().await;
                    let mut guard = lock_session(&session_b);
                    insert_on(&mut guard, berry_entity("other"));
                });
            });
        },
        PCT_DEPTH,
        PCT_ITERATIONS,
    );
}

#[tokio::test]
async fn tokio_graph_execute_branch_matches_ops() {
    let cgs = load_pokeapi_mini_cgs();
    let sess = test_execute_session(cgs, "branch_parity");
    let mut branch = GraphExecuteBranch::fork(&sess).await;
    branch
        .mat_mut()
        .insert(berry_entity("parity"))
        .expect("branch insert");
    branch.commit(&sess).await.expect("commit");
    let guard = sess.lock_graph_cache().await;
    assert!(guard.get(&Ref::new("Berry", "parity")).is_some());
}

#[tokio::test]
async fn tokio_spill_apply_while_commit() {
    let cgs = load_pokeapi_mini_cgs();
    let sess = Arc::new(test_execute_session(cgs.clone(), "spill_interleave"));
    let host = SpillHostFixture::new();

    {
        let mut guard = sess.lock_graph_cache().await;
        guard.insert(berry_entity("cheri")).expect("hot");
        guard.insert(berry_entity("pecha")).expect("hot");
    }
    spill_one_page(
        &host,
        sess.prompt_hash.as_str(),
        "spill_sid",
        vec![berry_entity("oran")],
    )
    .await;

    let sess_apply = Arc::clone(&sess);
    let host_apply = host.clone();
    let cgs_apply = Arc::clone(&cgs);
    let apply = tokio::spawn(async move {
        let mut result = empty_graph_result(3);
        {
            let guard = sess_apply.lock_graph_cache().await;
            GraphSurfaceRehydrator::sync_result_from_materialization(
                guard.materialization(),
                &sess_apply,
                host_apply.st.as_ref(),
                "spill_sid",
                "Berry",
                cgs_apply.as_ref(),
                &mut result,
            )
            .await;
        }
        result.entities.len()
    });

    let sess_commit = Arc::clone(&sess);
    let commit = tokio::spawn(async move {
        let mut branch = GraphExecuteBranch::fork(&sess_commit).await;
        branch
            .mat_mut()
            .insert(berry_entity("commit_side"))
            .expect("insert");
        branch.commit(&sess_commit).await.is_ok()
    });

    let (entity_count, _commit_ok) = tokio::join!(apply, commit);
    assert_eq!(entity_count.expect("apply task"), 3);
}

fn empty_graph_result(count: usize) -> plasm_runtime::ExecutionResult {
    plasm_runtime::ExecutionResult {
        entities: Vec::new(),
        count,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: plasm_runtime::ExecutionSource::Live,
        stats: plasm_runtime::ExecutionStats::default(),
        request_fingerprints: Vec::new(),
    }
}
