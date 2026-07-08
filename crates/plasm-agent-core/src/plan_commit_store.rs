//! Plan commit registration with optional durable descriptor refresh.

use std::sync::Arc;

use plasm_core::{PlanCommitId, PlanCommitRef};

use crate::execute_session::ExecuteSession;
pub use crate::mcp_transport_store::execute_session_registry::{
    ExecuteSessionPersistError, ExecuteSessionPersistOutcome,
};
use crate::operation::{
    compute_plan_commit_id_from_semantic, plan_commit_canonical_comp, PlanCommitDryCache,
    PlanCommitRecord,
};
use crate::plan_dry_display::{PlanDryReview, PlanDryVerdict};
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_plan_run::{evaluate_plasm_comp_dry, DryPlasmPlanEvaluation};
use crate::server_state::PlasmHostState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanCommitVerifyError {
    Unknown {
        commit_ref: PlanCommitRef,
    },
    Expired {
        commit_ref: PlanCommitRef,
    },
    Mismatch {
        commit_ref: PlanCommitRef,
    },
    /// Plan was pinned to a newer exposure than this session row (split-brain).
    PlanAheadOfSession {
        commit_ref: PlanCommitRef,
        plan_domain_revision: u32,
        session_domain_revision: u32,
    },
    StalePolicy {
        commit_ref: PlanCommitRef,
    },
    Evidence {
        commit_ref: PlanCommitRef,
        detail: String,
    },
}

impl PlanCommitVerifyError {
    pub fn detail(&self) -> String {
        self.to_string()
    }
}

impl std::fmt::Display for PlanCommitVerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown { commit_ref } => write!(
                f,
                "unknown plan_commit_ref `{commit_ref}` — call `plasm` dry-run again"
            ),
            Self::Expired { commit_ref } => write!(
                f,
                "plan_commit_ref `{commit_ref}` expired — call `plasm` dry-run again"
            ),
            Self::Mismatch { commit_ref } => write!(
                f,
                "plan_commit_ref `{commit_ref}` does not match the current program — call `plasm` dry-run again"
            ),
            Self::PlanAheadOfSession {
                commit_ref,
                plan_domain_revision,
                session_domain_revision,
            } => write!(
                f,
                "plan_commit_ref `{commit_ref}` is from a newer exposure (plan domain_revision={plan_domain_revision}, session domain_revision={session_domain_revision}) — session row is behind the plan; retry after rehydrate, or call `plasm` dry-run again on this session"
            ),
            Self::StalePolicy { commit_ref } => write!(
                f,
                "plan_commit_ref `{commit_ref}` is stale after flow policy changed — call `plasm` dry-run again"
            ),
            Self::Evidence { commit_ref, detail } => write!(
                f,
                "plan_commit_ref `{commit_ref}` evidence mismatch: {detail}"
            ),
        }
    }
}

impl std::error::Error for PlanCommitVerifyError {}

pub async fn register_plan_commit_and_persist(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    record: PlanCommitRecord,
) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
    register_plan_commit_with_persist(st, prompt_hash, session_id, record, true).await
}

/// Register a plan commit in the live execute session; optionally await durable descriptor patch.
pub async fn register_plan_commit_with_persist(
    st: &PlasmHostState,
    prompt_hash: &str,
    session_id: &str,
    mut record: PlanCommitRecord,
    await_persist: bool,
) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
    // Live row only — never fall back to a caller Arc (stale exposure / split-brain).
    let Some(live) = st.get_execute_session(prompt_hash, session_id).await else {
        return Err(ExecuteSessionPersistError::SessionUnavailable);
    };
    record.domain_revision = live.domain_revision;
    let commit_ref = record.commit_ref.clone();
    live.register_plan_commit(record);
    let reuse_key = st
        .sessions
        .reuse_key_for_execute_pair(prompt_hash, session_id)
        .await;
    let registry = st.execute_session_registry.clone();
    let es = Arc::clone(&live);
    let sid = session_id.to_string();
    if await_persist {
        match registry
            .patch_plan_commits_only(st, es.as_ref(), sid.as_str(), reuse_key.as_ref())
            .await
        {
            Ok(outcome) => Ok(outcome),
            Err(err) => {
                live.remove_plan_commit(&commit_ref);
                Err(err)
            }
        }
    } else {
        crate::metrics::record_mcp_response_deferred_io("commit_persist");
        let st_bg = st.clone();
        tokio::spawn(async move {
            if let Err(err) = registry
                .patch_plan_commits_only(&st_bg, es.as_ref(), sid.as_str(), reuse_key.as_ref())
                .await
            {
                tracing::warn!(
                    target: "plasm_agent::mcp",
                    error = %err,
                    "background plan commit persist failed (non-fatal)"
                );
            }
        });
        Ok(ExecuteSessionPersistOutcome::InMemoryOnly)
    }
}

pub fn verify_plan_commit_id(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
    commit_id: PlanCommitId,
) -> Result<(), PlanCommitVerifyError> {
    let Some(record) = es.get_plan_commit(commit_ref) else {
        return Err(PlanCommitVerifyError::Unknown {
            commit_ref: commit_ref.clone(),
        });
    };
    if record.is_expired() {
        return Err(PlanCommitVerifyError::Expired {
            commit_ref: commit_ref.clone(),
        });
    }
    if !crate::domain_revision::plan_compatible_with_session(
        crate::domain_revision::DomainRevision::new(record.domain_revision),
        crate::domain_revision::DomainRevision::new(es.domain_revision),
    ) {
        return Err(PlanCommitVerifyError::PlanAheadOfSession {
            commit_ref: commit_ref.clone(),
            plan_domain_revision: record.domain_revision,
            session_domain_revision: es.domain_revision,
        });
    }
    if record.policy_revision != es.flow_policy.revision_or_default() {
        return Err(PlanCommitVerifyError::StalePolicy {
            commit_ref: commit_ref.clone(),
        });
    }
    if commit_id != record.commit_id {
        return Err(PlanCommitVerifyError::Mismatch {
            commit_ref: commit_ref.clone(),
        });
    }
    if let Some(evidence) = crate::evidence_chain::chain(es) {
        evidence
            .verify_comp_commit_matches(&record.commit_id)
            .map_err(|e| PlanCommitVerifyError::Evidence {
                commit_ref: commit_ref.clone(),
                detail: e.to_string(),
            })?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct AcceptedPlanCommit {
    pub record: Option<PlanCommitRecord>,
    pub verdict_for_gate: PlanDryVerdict,
    pub review_for_delivery: PlanDryReview,
}

#[derive(Clone)]
pub struct CommittedPlan {
    pub commit_ref: PlanCommitRef,
    pub artifact: crate::plasm_comp_wire::PlasmCompArtifact,
    pub program: String,
    pub dry_review: PlanDryReview,
    pub verdict: PlanDryVerdict,
    pub dry_cache: PlanCommitDryCache,
}

impl CommittedPlan {
    #[must_use]
    pub fn lowered_ir_digest(&self) -> &str {
        self.dry_cache.lowered_ir_digest.as_str()
    }

    #[must_use]
    pub fn schedule_digest(&self) -> &str {
        self.dry_cache.schedule_digest.as_str()
    }
}

/// Prove live bundle matches the reviewed [`CommittedPlan`] (semantic commit id + lowered IR digest).
pub fn verify_committed_plan_bundle(
    bundle: &PlasmCompBundle,
    committed: &CommittedPlan,
) -> Result<(), PlanCommitVerifyError> {
    let live_id =
        compute_plan_commit_id_from_semantic(&plan_commit_canonical_comp(&bundle.artifact().comp));
    let stored_id =
        compute_plan_commit_id_from_semantic(&plan_commit_canonical_comp(&committed.artifact.comp));
    if live_id != stored_id {
        return Err(PlanCommitVerifyError::Mismatch {
            commit_ref: committed.commit_ref.clone(),
        });
    }
    if !committed.lowered_ir_digest().is_empty() || !committed.schedule_digest().is_empty() {
        let prepared = crate::plan_prepare::build_prepared_validated_plan(
            &bundle.artifact().comp,
            bundle.executable(),
        )
        .map_err(|e| PlanCommitVerifyError::Evidence {
            commit_ref: committed.commit_ref.clone(),
            detail: e,
        })?;
        if !committed.lowered_ir_digest().is_empty() {
            let live =
                crate::plasm_plan_run::lowered_ir_digest_from_validated_plan(prepared.artifact());
            if live.as_str() != committed.lowered_ir_digest() {
                return Err(PlanCommitVerifyError::Evidence {
                    commit_ref: committed.commit_ref.clone(),
                    detail: "lowered GET IR digest mismatch — call `plasm` dry-run again".into(),
                });
            }
        }
        if !committed.schedule_digest().is_empty() {
            // PEC seal: reclassify the rehydrated plan into `ExecStep`s over the *stored* topological
            // order and confirm the executable schedule is byte-identical to the reviewed dry-run.
            // A mismatch means the pure/io lowering itself drifted (e.g. redeploy) — refuse to run a
            // schedule the operator never reviewed rather than silently diverge.
            let live_schedule = crate::plasm_plan_run::ScheduleDigest::from_validated_plan(
                prepared.artifact(),
                &committed.dry_cache.topological_order,
            )
            .to_hex();
            if live_schedule != committed.schedule_digest() {
                return Err(PlanCommitVerifyError::Evidence {
                    commit_ref: committed.commit_ref.clone(),
                    detail: "executable schedule digest mismatch — call `plasm` dry-run again"
                        .into(),
                });
            }
        }
    }
    Ok(())
}

/// Dry evaluation for `plasm_run`: reuse commit cache when populated; otherwise evaluate once.
pub fn dry_for_committed_plasm_run(
    es: &ExecuteSession,
    bundle: &PlasmCompBundle,
    committed: &CommittedPlan,
) -> Result<DryPlasmPlanEvaluation, PlanCommitVerifyError> {
    verify_committed_plan_bundle(bundle, committed)?;
    let map_dry = |detail: String| PlanCommitVerifyError::Evidence {
        commit_ref: committed.commit_ref.clone(),
        detail,
    };
    if committed.dry_cache.is_populated() {
        DryPlasmPlanEvaluation::from_plan_commit_cache(
            es,
            bundle,
            &committed.dry_cache,
            committed.dry_review.clone(),
        )
        .map_err(map_dry)
    } else {
        evaluate_plasm_comp_dry(es, bundle).map_err(map_dry)
    }
}

pub fn resolve_committed_plan(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
) -> Result<CommittedPlan, PlanCommitVerifyError> {
    let record = es
        .get_plan_commit(commit_ref)
        .ok_or_else(|| PlanCommitVerifyError::Unknown {
            commit_ref: commit_ref.clone(),
        })?;
    if record.is_expired() {
        return Err(PlanCommitVerifyError::Expired {
            commit_ref: commit_ref.clone(),
        });
    }
    let commit_id =
        compute_plan_commit_id_from_semantic(&plan_commit_canonical_comp(&record.artifact.comp));
    verify_plan_commit_id(es, commit_ref, commit_id)?;
    Ok(CommittedPlan {
        commit_ref: record.commit_ref,
        artifact: record.artifact,
        program: record.program,
        dry_review: record.dry_review,
        verdict: record.verdict,
        dry_cache: record.dry_cache,
    })
}

pub fn accept_plan_commit_for_bundle(
    es: &ExecuteSession,
    commit_ref: Option<&PlanCommitRef>,
    bundle: &PlasmCompBundle,
    fresh_verdict: PlanDryVerdict,
    fresh_review: &PlanDryReview,
) -> Result<AcceptedPlanCommit, PlanCommitVerifyError> {
    let Some(commit_ref) = commit_ref else {
        return Ok(AcceptedPlanCommit {
            record: None,
            verdict_for_gate: fresh_verdict,
            review_for_delivery: fresh_review.clone(),
        });
    };
    let commit_id =
        compute_plan_commit_id_from_semantic(&plan_commit_canonical_comp(&bundle.artifact().comp));
    verify_plan_commit_id(es, commit_ref, commit_id)?;
    let record = es
        .get_plan_commit(commit_ref)
        .ok_or_else(|| PlanCommitVerifyError::Unknown {
            commit_ref: commit_ref.clone(),
        })?;
    if record.is_expired() {
        return Err(PlanCommitVerifyError::Expired {
            commit_ref: commit_ref.clone(),
        });
    }
    Ok(AcceptedPlanCommit {
        verdict_for_gate: record.verdict,
        review_for_delivery: record.dry_review.clone(),
        record: Some(record),
    })
}

#[cfg(test)]
#[path = "plan_commit_coherence_tests.rs"]
mod coherence_tests;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use plasm_core::{CgsContext, CGS};

    use super::{
        accept_plan_commit_for_bundle, dry_for_committed_plasm_run, verify_committed_plan_bundle,
        verify_plan_commit_id, CommittedPlan, PlanCommitVerifyError,
    };
    use crate::execute_session::ExecuteSession;
    use crate::operation::{
        plan_commit_canonical_comp, PlanCommitDryCache, PlanCommitRecord, RehydratedPlanCommit,
        PLAN_COMMIT_TTL,
    };
    use crate::plan_dry_display::{PlanDryReview, PlanDryVerdict};
    use crate::plan_flow_policy::PolicyRevision;
    use crate::plasm_comp_bundle::PlasmCompBundle;
    use plasm_core::{PlanCommitId, PlanCommitRef};

    #[allow(clippy::too_many_arguments)]
    pub(super) fn rehydrate_record(
        es: &ExecuteSession,
        commit_ref: PlanCommitRef,
        commit_id: PlanCommitId,
        domain_revision: u32,
        policy_revision: PolicyRevision,
        artifact: crate::plasm_comp_wire::PlasmCompArtifact,
        program: String,
        dry_review: crate::plan_dry_display::PlanDryReview,
        verdict: PlanDryVerdict,
        expires_at: std::time::Instant,
        dry_cache: crate::operation::PlanCommitDryCache,
    ) -> PlanCommitRecord {
        PlanCommitRecord::rehydrated_from_persisted(
            es,
            RehydratedPlanCommit {
                commit_ref,
                commit_id,
                domain_revision,
                policy_revision,
                artifact,
                program,
                dry_review,
                verdict,
                expires_at,
                dry_cache,
            },
        )
        .expect("rehydrate")
    }

    pub(super) fn minimal_session() -> ExecuteSession {
        let cgs = Arc::new(CGS::new());
        let mut ctxs = indexmap::IndexMap::new();
        ctxs.insert(
            "default".into(),
            Arc::new(CgsContext::entry("default", cgs.clone())),
        );
        ExecuteSession::new(
            "ph".into(),
            "p".into(),
            cgs,
            ctxs,
            "default".into(),
            String::new(),
            String::new(),
            None,
            vec!["Pet".into()],
            None,
            None,
            "hash".into(),
            None,
            None,
        )
    }

    pub(super) fn minimal_artifact() -> crate::plasm_comp_wire::PlasmCompArtifact {
        use plasm_core::plasm_monad::*;
        use std::collections::BTreeMap;

        let mut steps = BTreeMap::new();
        steps.insert(
            "x".into(),
            PlasmStepPayload::Pure(PurePayload {
                data: plasm_core::PlasmDataValue::Literal {
                    value: serde_json::json!("ok"),
                },
                effect_class: plasm_core::EffectClass::ArtifactRead,
                result_shape: plasm_core::ResultShape::Artifact,
            }),
        );
        crate::plasm_comp_wire::PlasmCompArtifact {
            comp: PlasmComp {
                version: 1,
                name: Some("test".into()),
                steps,
                bind: PlasmBindGraph {
                    topo: vec![StepId::new("x").expect("step id")],
                    deps: Default::default(),
                    primary: Default::default(),
                    holes: Default::default(),
                },
                return_: PlasmReturn::Step {
                    step: StepId::new("x").expect("return step"),
                },
                metadata: Default::default(),
            },
            approval_gates: Vec::new(),
        }
    }

    #[test]
    fn verify_splits_unknown_expired_and_mismatch() {
        let es = minimal_session();
        let pc = es.mint_plan_commit_ref();
        let err = verify_plan_commit_id(&es, &pc, PlanCommitId::from_canonical_bytes([0u8; 32]))
            .expect_err("unknown");
        assert!(matches!(err, PlanCommitVerifyError::Unknown { .. }));

        let record = rehydrate_record(
            &es,
            pc.clone(),
            PlanCommitId::from_canonical_bytes([1u8; 32]),
            0,
            es.flow_policy.revision_or_default(),
            minimal_artifact(),
            "test".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() - PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        );
        es.register_plan_commit(record);
        let err = verify_plan_commit_id(&es, &pc, PlanCommitId::from_canonical_bytes([1u8; 32]))
            .expect_err("expired");
        assert!(matches!(err, PlanCommitVerifyError::Expired { .. }));

        es.register_plan_commit(rehydrate_record(
            &es,
            pc.clone(),
            PlanCommitId::from_canonical_bytes([2u8; 32]),
            0,
            es.flow_policy.revision_or_default(),
            minimal_artifact(),
            "test".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ));
        let err = verify_plan_commit_id(&es, &pc, PlanCommitId::from_canonical_bytes([9u8; 32]))
            .expect_err("mismatch");
        assert!(matches!(err, PlanCommitVerifyError::Mismatch { .. }));
    }

    #[test]
    fn domain_revision_extend_keeps_prior_plan_commit() {
        let mut es = minimal_session();
        let pc = es.mint_plan_commit_ref();
        let commit_id = PlanCommitId::from_canonical_bytes([4u8; 32]);
        es.register_plan_commit(rehydrate_record(
            &es,
            pc.clone(),
            commit_id.clone(),
            0,
            es.flow_policy.revision_or_default(),
            minimal_artifact(),
            "test".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ));
        // Append-only: extend advances session exposure; prior pcN stays valid.
        es.domain_revision = 1;
        verify_plan_commit_id(&es, &pc, commit_id).expect("plan survives extend");
    }

    #[test]
    fn domain_revision_rejects_plan_ahead_of_session() {
        let mut es = minimal_session();
        let pc = es.mint_plan_commit_ref();
        let commit_id = PlanCommitId::from_canonical_bytes([5u8; 32]);
        es.register_plan_commit(rehydrate_record(
            &es,
            pc.clone(),
            commit_id.clone(),
            3,
            es.flow_policy.revision_or_default(),
            minimal_artifact(),
            "test".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ));
        es.domain_revision = 2;
        let err = verify_plan_commit_id(&es, &pc, commit_id).expect_err("plan ahead of session");
        assert!(matches!(
            err,
            PlanCommitVerifyError::PlanAheadOfSession {
                plan_domain_revision: 3,
                session_domain_revision: 2,
                ..
            }
        ));
        let detail = err.detail();
        assert!(
            detail.contains("newer exposure")
                && detail.contains("plan domain_revision=3")
                && detail.contains("session domain_revision=2"),
            "detail={detail}"
        );
    }

    #[test]
    fn policy_revision_mismatch_rejects_stale_pc() {
        let mut es = minimal_session();
        let pc = es.mint_plan_commit_ref();
        let commit_id = PlanCommitId::from_canonical_bytes([6u8; 32]);
        es.register_plan_commit(rehydrate_record(
            &es,
            pc.clone(),
            commit_id.clone(),
            0,
            es.flow_policy.revision_or_default(),
            minimal_artifact(),
            "test".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ));
        es.flow_policy = crate::FlowPolicySnapshot::Active {
            revision: crate::PolicyRevision(2),
            policy: crate::FlowPolicy::default(),
        };
        let err = verify_plan_commit_id(&es, &pc, commit_id).expect_err("stale policy");
        assert!(matches!(err, PlanCommitVerifyError::StalePolicy { .. }));
    }

    #[test]
    fn register_roundtrip_on_session() {
        let es = minimal_session();
        let pc = es.mint_plan_commit_ref();
        let commit_id = PlanCommitId::from_canonical_bytes([7u8; 32]);
        es.register_plan_commit(rehydrate_record(
            &es,
            pc.clone(),
            commit_id.clone(),
            0,
            es.flow_policy.revision_or_default(),
            minimal_artifact(),
            "test".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ));
        verify_plan_commit_id(&es, &pc, commit_id).expect("roundtrip");
    }

    #[test]
    fn accept_plan_commit_uses_stored_review_for_matching_bundle() {
        let es = minimal_session();
        let artifact = minimal_artifact();
        let bundle = PlasmCompBundle::new(artifact.clone()).expect("bundle");
        let pc = es.mint_plan_commit_ref();
        let commit_id = crate::operation::compute_plan_commit_id_from_semantic(
            &plan_commit_canonical_comp(&artifact.comp),
        );
        let review = PlanDryReview {
            has_unbounded_read_root: true,
            ..Default::default()
        };
        es.register_plan_commit(rehydrate_record(
            &es,
            pc.clone(),
            commit_id,
            es.domain_revision,
            es.flow_policy.revision_or_default(),
            artifact,
            "e1.limit(3)".into(),
            review.clone(),
            PlanDryVerdict::Review,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ));

        let accepted = accept_plan_commit_for_bundle(
            &es,
            Some(&pc),
            &bundle,
            PlanDryVerdict::Ok,
            &PlanDryReview::default(),
        )
        .expect("accepted");

        assert_eq!(accepted.verdict_for_gate, PlanDryVerdict::Review);
        assert!(accepted.review_for_delivery.has_unbounded_read_root);
        assert_eq!(
            accepted.record.as_ref().map(|r| r.program.as_str()),
            Some("e1.limit(3)")
        );
    }

    #[test]
    fn dry_for_committed_reuses_plan_commit_cache() {
        let es = minimal_session();
        let artifact = minimal_artifact();
        let bundle = PlasmCompBundle::new(artifact.clone()).expect("bundle");
        let cache = PlanCommitDryCache {
            topological_order: vec!["items".into()],
            node_results: vec![serde_json::json!({"ok": true})],
            graph_summary: serde_json::json!({"nodes": 1}),
            ..PlanCommitDryCache::default()
        };
        let committed = CommittedPlan {
            commit_ref: es.mint_plan_commit_ref(),
            artifact: artifact.clone(),
            program: "e1".into(),
            dry_review: Default::default(),
            verdict: PlanDryVerdict::Ok,
            dry_cache: cache.clone(),
        };
        let dry = dry_for_committed_plasm_run(&es, &bundle, &committed).expect("hydrated dry");
        assert_eq!(dry.topological_order, cache.topological_order);
        assert_eq!(dry.node_results.len(), 1);
        verify_committed_plan_bundle(&bundle, &committed).expect("bundle matches");
    }

    #[test]
    fn verify_seals_executable_schedule_digest() {
        let es = minimal_session();
        let artifact = minimal_artifact();
        let bundle = PlasmCompBundle::new(artifact.clone()).expect("bundle");
        let order = vec!["items".to_string()];

        // The digest the reviewed dry-run would seal: classify the rehydrated plan over the stored
        // topological order.
        let prepared = crate::plan_prepare::build_prepared_validated_plan(
            &bundle.artifact().comp,
            bundle.executable(),
        )
        .expect("prepared");
        let good_digest =
            crate::plasm_plan_run::ScheduleDigest::from_validated_plan(prepared.artifact(), &order)
                .to_hex();
        assert!(!good_digest.is_empty(), "schedule digest must be non-empty");

        let committed = |schedule_digest: String| CommittedPlan {
            commit_ref: es.mint_plan_commit_ref(),
            artifact: artifact.clone(),
            program: "e1".into(),
            dry_review: Default::default(),
            verdict: PlanDryVerdict::Ok,
            dry_cache: PlanCommitDryCache {
                topological_order: order.clone(),
                schedule_digest,
                ..PlanCommitDryCache::default()
            },
        };

        // Matching schedule digest verifies.
        verify_committed_plan_bundle(&bundle, &committed(good_digest.clone()))
            .expect("matching schedule digest passes the seal");

        // A drifted lowering (tampered digest) is refused rather than silently executed.
        let err = verify_committed_plan_bundle(&bundle, &committed("00".repeat(32)))
            .expect_err("drifted schedule digest must fail the seal");
        match err {
            PlanCommitVerifyError::Evidence { detail, .. } => {
                assert!(
                    detail.contains("executable schedule digest mismatch"),
                    "unexpected detail: {detail}"
                );
            }
            other => panic!("expected Evidence mismatch, got {other:?}"),
        }
    }
}
