//! Plan commit registration with optional durable descriptor refresh.

use plasm_core::{PlanCommitId, PlanCommitRef};

use crate::execute_session::ExecuteSession;
use crate::mcp_transport_store::execute_session_registry::{
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
        commit_ref: String,
    },
    Expired {
        commit_ref: String,
    },
    Mismatch {
        commit_ref: String,
    },
    StaleDomain {
        commit_ref: String,
        plan_domain_revision: u32,
        session_domain_revision: u32,
    },
    StalePolicy {
        commit_ref: String,
    },
    Evidence {
        commit_ref: String,
        detail: String,
    },
}

impl PlanCommitVerifyError {
    pub fn detail(&self) -> String {
        match self {
            Self::Unknown { commit_ref } => format!(
                "unknown plan_commit_ref `{commit_ref}` — call `plasm` dry-run again"
            ),
            Self::Expired { commit_ref } => format!(
                "plan_commit_ref `{commit_ref}` expired — call `plasm` dry-run again"
            ),
            Self::Mismatch { commit_ref } => format!(
                "plan_commit_ref `{commit_ref}` does not match the current program — call `plasm` dry-run again"
            ),
            Self::StaleDomain {
                commit_ref,
                plan_domain_revision,
                session_domain_revision,
            } => format!(
                "plan_commit_ref `{commit_ref}` is stale (plan domain_revision={plan_domain_revision}, session domain_revision={session_domain_revision}) — call `plasm` dry-run again; if symbols are missing, re-`plasm_context` extend first"
            ),
            Self::StalePolicy { commit_ref } => format!(
                "plan_commit_ref `{commit_ref}` is stale after flow policy changed — call `plasm` dry-run again"
            ),
            Self::Evidence { commit_ref, detail } => format!(
                "plan_commit_ref `{commit_ref}` evidence mismatch: {detail}"
            ),
        }
    }
}

pub async fn register_plan_commit_and_persist(
    st: &PlasmHostState,
    session: std::sync::Arc<ExecuteSession>,
    prompt_hash: &str,
    session_id: &str,
    record: PlanCommitRecord,
) -> Result<ExecuteSessionPersistOutcome, ExecuteSessionPersistError> {
    // Prefer the live execute row (exposure / domain_revision). Plan commits use a shared map on
    // clones of that row; durable patch writes **only** plan_commits so a pre-extend Arc cannot
    // overwrite a newer teaching wave.
    let live = st
        .get_execute_session(prompt_hash, session_id)
        .await
        .unwrap_or_else(|| std::sync::Arc::clone(&session));
    live.register_plan_commit(record.clone());
    let reuse_key = st
        .sessions
        .reuse_key_for_execute_pair(prompt_hash, session_id)
        .await;
    match st
        .execute_session_registry
        .patch_plan_commits_only(live.as_ref(), session_id, reuse_key.as_ref())
        .await
    {
        Ok(outcome) => Ok(outcome),
        Err(err) => {
            live.remove_plan_commit(&record.commit_ref);
            Err(err)
        }
    }
}

pub fn verify_plan_commit_id(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
    commit_id: PlanCommitId,
) -> Result<(), PlanCommitVerifyError> {
    let Some(record) = es.get_plan_commit(commit_ref) else {
        return Err(PlanCommitVerifyError::Unknown {
            commit_ref: commit_ref.as_str().to_string(),
        });
    };
    if record.is_expired() {
        return Err(PlanCommitVerifyError::Expired {
            commit_ref: commit_ref.as_str().to_string(),
        });
    }
    if record.domain_revision != es.domain_revision {
        return Err(PlanCommitVerifyError::StaleDomain {
            commit_ref: commit_ref.as_str().to_string(),
            plan_domain_revision: record.domain_revision,
            session_domain_revision: es.domain_revision,
        });
    }
    if record.policy_revision != es.flow_policy.revision_or_default() {
        return Err(PlanCommitVerifyError::StalePolicy {
            commit_ref: commit_ref.as_str().to_string(),
        });
    }
    if commit_id != record.commit_id {
        return Err(PlanCommitVerifyError::Mismatch {
            commit_ref: commit_ref.as_str().to_string(),
        });
    }
    if let Some(evidence) = crate::evidence_chain::chain(es) {
        evidence
            .verify_comp_commit_matches(&record.commit_id)
            .map_err(|e| PlanCommitVerifyError::Evidence {
                commit_ref: commit_ref.as_str().to_string(),
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
            commit_ref: committed.commit_ref.as_str().to_string(),
        });
    }
    if !committed.lowered_ir_digest().is_empty() {
        let prepared = crate::plan_prepare::build_prepared_validated_plan(
            &bundle.artifact().comp,
            bundle.executable(),
        )
        .map_err(|e| PlanCommitVerifyError::Evidence {
            commit_ref: committed.commit_ref.as_str().to_string(),
            detail: e,
        })?;
        let live =
            crate::plasm_plan_run::lowered_ir_digest_from_validated_plan(prepared.artifact());
        if live.as_str() != committed.lowered_ir_digest() {
            return Err(PlanCommitVerifyError::Evidence {
                commit_ref: committed.commit_ref.as_str().to_string(),
                detail: "lowered GET IR digest mismatch — call `plasm` dry-run again".into(),
            });
        }
    }
    Ok(())
}

/// Dry evaluation for `plasm_run`: reuse commit cache when populated; otherwise evaluate once.
pub fn dry_for_committed_plasm_run(
    es: &ExecuteSession,
    bundle: &PlasmCompBundle,
    committed: &CommittedPlan,
) -> Result<DryPlasmPlanEvaluation, String> {
    verify_committed_plan_bundle(bundle, committed).map_err(|e| e.detail())?;
    if committed.dry_cache.is_populated() {
        DryPlasmPlanEvaluation::from_plan_commit_cache(
            es,
            bundle,
            &committed.dry_cache,
            committed.dry_review.clone(),
        )
    } else {
        evaluate_plasm_comp_dry(es, bundle)
    }
}

pub fn resolve_committed_plan(
    es: &ExecuteSession,
    commit_ref: &PlanCommitRef,
) -> Result<CommittedPlan, PlanCommitVerifyError> {
    let record = es
        .get_plan_commit(commit_ref)
        .ok_or_else(|| PlanCommitVerifyError::Unknown {
            commit_ref: commit_ref.as_str().to_string(),
        })?;
    if record.is_expired() {
        return Err(PlanCommitVerifyError::Expired {
            commit_ref: commit_ref.as_str().to_string(),
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
            commit_ref: commit_ref.as_str().to_string(),
        })?;
    if record.is_expired() {
        return Err(PlanCommitVerifyError::Expired {
            commit_ref: commit_ref.as_str().to_string(),
        });
    }
    Ok(AcceptedPlanCommit {
        verdict_for_gate: record.verdict,
        review_for_delivery: record.dry_review.clone(),
        record: Some(record),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use plasm_core::{CgsContext, CGS};

    use super::{
        accept_plan_commit_for_bundle, dry_for_committed_plasm_run,
        register_plan_commit_and_persist, verify_committed_plan_bundle, verify_plan_commit_id,
        CommittedPlan, PlanCommitVerifyError,
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
    fn rehydrate_record(
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

    fn minimal_session() -> ExecuteSession {
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

    fn minimal_artifact() -> crate::plasm_comp_wire::PlasmCompArtifact {
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
    fn domain_revision_mismatch_rejects_stale_pc() {
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
        es.domain_revision = 1;
        let err = verify_plan_commit_id(&es, &pc, commit_id).expect_err("stale domain");
        assert!(matches!(
            err,
            PlanCommitVerifyError::StaleDomain {
                plan_domain_revision: 0,
                session_domain_revision: 1,
                ..
            }
        ));
        let detail = err.detail();
        assert!(
            detail.contains("plan domain_revision=0")
                && detail.contains("session domain_revision=1"),
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

    async fn open_overshow_profile_host() -> (
        crate::server_state::PlasmHostState,
        crate::http_execute::CreateExecuteSessionResponse,
        Arc<ExecuteSession>,
    ) {
        use std::path::Path;

        use plasm_core::discovery::InMemoryCgsRegistry;
        use plasm_core::loader::load_schema_dir;
        use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};

        use crate::http::{build_plasm_host_state, PlasmHostBootstrap};
        use crate::http_execute::{execute_session_create_response, CreateExecuteSessionBody};
        use crate::mcp_transport_store::ExecuteSessionRegistry;
        use crate::run_artifacts::RunArtifactStore;
        use crate::server_state::CatalogBootstrap;

        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let registry = Arc::new(InMemoryCgsRegistry::from_pairs(vec![(
            "overshow".into(),
            "Overshow".into(),
            vec!["demo".into()],
            cgs,
        )]));
        let (execute_registry, _) = ExecuteSessionRegistry::with_test_json_store();
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        let mut host = build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry,
            catalog_bootstrap: CatalogBootstrap::Fixed,
            incoming_auth: None,
            run_artifacts: Arc::new(RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        });
        host.oss.execute_session_registry = execute_registry;

        let created = execute_session_create_response(
            &host,
            None,
            CreateExecuteSessionBody {
                entry_id: "overshow".into(),
                entities: vec!["Profile".into()],
                principal: None,
                logical_session_id: None,
                context_intent: None,
                ranked_capabilities: None,
                read_first_seeded_exposure: false,
            },
        )
        .await
        .expect("open session");
        let es = host
            .get_execute_session(&created.prompt_hash, &created.session)
            .await
            .expect("session row");
        (host, created, es)
    }

    #[tokio::test]
    async fn register_persist_survives_rehydrate() {
        let (host, created, es) = open_overshow_profile_host().await;
        let pc = es.mint_plan_commit_ref();
        let artifact = minimal_artifact();
        let commit_id = crate::operation::compute_plan_commit_id_from_semantic(
            &plan_commit_canonical_comp(&artifact.comp),
        );
        register_plan_commit_and_persist(
            &host,
            es.clone(),
            created.prompt_hash.as_str(),
            created.session.as_str(),
            rehydrate_record(
                &es,
                pc.clone(),
                commit_id.clone(),
                es.domain_revision,
                es.flow_policy.revision_or_default(),
                artifact.clone(),
                "test".into(),
                Default::default(),
                PlanDryVerdict::Ok,
                std::time::Instant::now() + PLAN_COMMIT_TTL,
                PlanCommitDryCache::default(),
            ),
        )
        .await
        .expect("persist plan commit");
        host.sessions.purge_all().await;
        let es2 = host
            .get_execute_session(&created.prompt_hash, &created.session)
            .await
            .expect("rehydrate");
        verify_plan_commit_id(&es2, &pc, commit_id).expect("persisted plan commit");
        let record = es2.get_plan_commit(&pc).expect("persisted record");
        let bundle = PlasmCompBundle::new(record.artifact).expect("persisted artifact bundle");
        let accepted = accept_plan_commit_for_bundle(
            &es2,
            Some(&pc),
            &bundle,
            PlanDryVerdict::Review,
            &PlanDryReview::default(),
        )
        .expect("rehydrated token accepted");
        assert_eq!(accepted.verdict_for_gate, PlanDryVerdict::Ok);
        assert_eq!(
            accepted.record.as_ref().map(|r| r.program.as_str()),
            Some("test")
        );
    }

    /// Stale pre-extend Arc must not overwrite durable exposure / domain_revision when registering pcN.
    #[tokio::test]
    async fn register_plan_commit_does_not_overwrite_durable_exposure() {
        let (host, created, live) = open_overshow_profile_host().await;
        // Simulate extend: live row advances domain_revision and is fully persisted.
        let mut extended = (*live).clone();
        extended.domain_revision = 7;
        extended.entities = vec!["Profile".into(), "Extra".into()];
        extended.prompt_text.push_str("\n# e2 Extra\n");
        host.replace_execute_session(
            created.prompt_hash.as_str(),
            created.session.as_str(),
            extended.clone(),
        )
        .await;
        let reuse_key = host
            .sessions
            .reuse_key_for_execute_pair(created.prompt_hash.as_str(), created.session.as_str())
            .await;
        host.execute_session_registry
            .persist_or_update(&extended, created.session.as_str(), reuse_key.as_ref())
            .await
            .expect("persist extended");

        // Pre-extend Arc (stale exposure) registers a plan commit — must not clobber durable wave.
        assert_eq!(live.domain_revision, 0);
        let pc = live.mint_plan_commit_ref();
        let artifact = minimal_artifact();
        let commit_id = crate::operation::compute_plan_commit_id_from_semantic(
            &plan_commit_canonical_comp(&artifact.comp),
        );
        register_plan_commit_and_persist(
            &host,
            live,
            created.prompt_hash.as_str(),
            created.session.as_str(),
            rehydrate_record(
                &extended,
                pc.clone(),
                commit_id,
                7,
                extended.flow_policy.revision_or_default(),
                artifact,
                "stale-arc".into(),
                Default::default(),
                PlanDryVerdict::Ok,
                std::time::Instant::now() + PLAN_COMMIT_TTL,
                PlanCommitDryCache::default(),
            ),
        )
        .await
        .expect("patch plan commits");

        let durable = host
            .execute_session_registry
            .load(created.prompt_hash.as_str(), created.session.as_str())
            .await
            .expect("durable row");
        assert_eq!(
            durable.domain_revision, 7,
            "stale Arc must not overwrite domain_revision"
        );
        assert_eq!(
            durable.entities,
            vec!["Profile".to_string(), "Extra".to_string()],
            "stale Arc must not overwrite entities"
        );
        assert!(
            durable
                .plan_commits
                .iter()
                .any(|r| r.commit_ref == pc.as_str()),
            "plan commit must still be patched"
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
}
