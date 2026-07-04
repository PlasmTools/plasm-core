//! Durable/hot exposure coherence for plan commits (append-only domain_revision).

use std::sync::Arc;

use super::tests::{minimal_artifact, rehydrate_record};
use super::{
    accept_plan_commit_for_bundle, register_plan_commit_and_persist, verify_plan_commit_id,
};
use crate::execute_session::ExecuteSession;
use crate::mcp_transport_store::execute_session_registry::ExecuteSessionPersistError;
use crate::operation::{plan_commit_canonical_comp, PlanCommitDryCache, PLAN_COMMIT_TTL};
use crate::plan_dry_display::{PlanDryReview, PlanDryVerdict};
use crate::plasm_comp_bundle::PlasmCompBundle;

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

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
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

/// Register always uses live row — never a caller Arc — so extend exposure is preserved.
#[tokio::test]
async fn register_plan_commit_uses_live_exposure_not_caller_arc() {
    let (host, created, live) = open_overshow_profile_host().await;
    let mut extended = (*live).clone();
    extended.domain_revision = 7;
    extended.entities = vec!["Profile".into(), "Extra".into()];
    extended.prompt_text.push_str("\n# e2 Extra\n");
    host.replace_execute_session(
        created.prompt_hash.as_str(),
        created.session.as_str(),
        extended.clone(),
    )
    .await
    .expect("persist extended");

    assert_eq!(live.domain_revision, 0);
    let live_now = host
        .get_execute_session(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("live");
    let pc = live_now.mint_plan_commit_ref();
    let artifact = minimal_artifact();
    let commit_id = crate::operation::compute_plan_commit_id_from_semantic(
        &plan_commit_canonical_comp(&artifact.comp),
    );
    register_plan_commit_and_persist(
        &host,
        created.prompt_hash.as_str(),
        created.session.as_str(),
        rehydrate_record(
            &live_now,
            pc.clone(),
            commit_id,
            7,
            live_now.flow_policy.revision_or_default(),
            artifact,
            "live-row".into(),
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
    assert_eq!(durable.domain_revision, 7);
    assert_eq!(
        durable.entities,
        vec!["Profile".to_string(), "Extra".to_string()]
    );
    assert!(durable
        .plan_commits
        .iter()
        .any(|r| r.commit_ref == pc.as_str()));
}

#[tokio::test]
async fn register_fails_closed_when_session_unavailable() {
    let (host, created, es) = open_overshow_profile_host().await;
    host.sessions.purge_all().await;
    host.execute_session_registry
        .delete(created.prompt_hash.as_str(), created.session.as_str())
        .await;
    let pc = es.mint_plan_commit_ref();
    let artifact = minimal_artifact();
    let commit_id = crate::operation::compute_plan_commit_id_from_semantic(
        &plan_commit_canonical_comp(&artifact.comp),
    );
    let err = register_plan_commit_and_persist(
        &host,
        created.prompt_hash.as_str(),
        created.session.as_str(),
        rehydrate_record(
            &es,
            pc,
            commit_id,
            0,
            es.flow_policy.revision_or_default(),
            artifact,
            "gone".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ),
    )
    .await
    .expect_err("no live session");
    assert!(matches!(
        err,
        ExecuteSessionPersistError::SessionUnavailable
    ));
}

/// Hot at older exposure must not absorb durable plans@N — full rehydrate to durable rev.
#[tokio::test]
async fn hot_behind_durable_rehydrates_to_plan_revision() {
    let (host, created, live) = open_overshow_profile_host().await;
    let mut extended = (*live).clone();
    extended.domain_revision = 3;
    extended.prompt_text.push_str("\n# wave 3\n");
    host.replace_execute_session(
        created.prompt_hash.as_str(),
        created.session.as_str(),
        extended.clone(),
    )
    .await
    .expect("persist extended");
    let live3 = host
        .get_execute_session(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("live at rev 3");
    let pc = live3.mint_plan_commit_ref();
    let artifact = minimal_artifact();
    let commit_id = crate::operation::compute_plan_commit_id_from_semantic(
        &plan_commit_canonical_comp(&artifact.comp),
    );
    register_plan_commit_and_persist(
        &host,
        created.prompt_hash.as_str(),
        created.session.as_str(),
        rehydrate_record(
            &live3,
            pc.clone(),
            commit_id.clone(),
            3,
            live3.flow_policy.revision_or_default(),
            artifact,
            "rev3-plan".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ),
    )
    .await
    .expect("plan at rev 3");

    let reuse_key = host
        .sessions
        .reuse_key_for_execute_pair(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("reuse key");
    let mut stale = (*live).clone();
    stale.domain_revision = 2;
    host.sessions.purge_all().await;
    host.sessions
        .insert(
            reuse_key,
            created.prompt_hash.clone(),
            created.session.clone(),
            stale,
        )
        .await;

    let recovered = host
        .get_execute_session(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("rehydrate past stale hot");
    assert_eq!(recovered.domain_revision, 3);
    assert!(
        recovered.prompt_text.contains("wave 3"),
        "rehydrate must restore durable exposure text"
    );
    verify_plan_commit_id(&recovered, &pc, commit_id).expect("plan survives rehydrate");
}

/// When hot exposure is ahead of durable, plan register full-persists.
#[tokio::test]
async fn plan_register_full_persists_when_hot_ahead_of_durable() {
    let (host, created, live) = open_overshow_profile_host().await;
    let mut hot = (*live).clone();
    hot.domain_revision = 3;
    hot.prompt_text.push_str("\n# hot-ahead wave\n");
    let reuse_key = host
        .sessions
        .reuse_key_for_execute_pair(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("reuse key");
    host.execute_session_registry
        .persist_or_update(live.as_ref(), created.session.as_str(), Some(&reuse_key))
        .await
        .expect("durable at open rev");
    if let (Ok(ph), Ok(sid)) = (
        created
            .prompt_hash
            .parse::<crate::execute_path_ids::PromptHashHex>(),
        created
            .session
            .parse::<crate::execute_path_ids::ExecuteSessionId>(),
    ) {
        host.sessions.replace_session(&ph, &sid, hot.clone()).await;
    }
    let durable_before = host
        .execute_session_registry
        .load(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("durable");
    assert_eq!(durable_before.domain_revision, live.domain_revision);

    let live_hot = host
        .get_execute_session(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("hot");
    assert_eq!(live_hot.domain_revision, 3);
    let pc = live_hot.mint_plan_commit_ref();
    let artifact = minimal_artifact();
    let commit_id = crate::operation::compute_plan_commit_id_from_semantic(
        &plan_commit_canonical_comp(&artifact.comp),
    );
    register_plan_commit_and_persist(
        &host,
        created.prompt_hash.as_str(),
        created.session.as_str(),
        rehydrate_record(
            &hot,
            pc.clone(),
            commit_id,
            3,
            hot.flow_policy.revision_or_default(),
            artifact,
            "hot-ahead".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ),
    )
    .await
    .expect("full persist");

    let durable = host
        .execute_session_registry
        .load(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("durable after plan");
    assert_eq!(durable.domain_revision, 3);
    assert!(durable.prompt_text.contains("hot-ahead wave"));
    assert!(durable
        .plan_commits
        .iter()
        .any(|r| r.commit_ref == pc.as_str() && r.domain_revision == 3));
}

/// Merge restores plans pinned at older revisions (append-only prefix).
#[tokio::test]
async fn merge_restores_plans_from_prior_exposure_revisions() {
    use crate::mcp_transport_store::execute_session_registry::MergeLiveOutcome;

    let (host, created, es) = open_overshow_profile_host().await;
    let open_rev = es.domain_revision;
    let pc0 = es.mint_plan_commit_ref();
    let artifact0 = minimal_artifact();
    let id0 = crate::operation::compute_plan_commit_id_from_semantic(
        &plan_commit_canonical_comp(&artifact0.comp),
    );
    register_plan_commit_and_persist(
        &host,
        created.prompt_hash.as_str(),
        created.session.as_str(),
        rehydrate_record(
            &es,
            pc0.clone(),
            id0.clone(),
            open_rev,
            es.flow_policy.revision_or_default(),
            artifact0,
            "at-open".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ),
    )
    .await
    .expect("pc0");

    let mut extended = (*es).clone();
    extended.domain_revision = open_rev.saturating_add(2);
    extended.prompt_text.push_str("\n# extended\n");
    host.replace_execute_session(
        created.prompt_hash.as_str(),
        created.session.as_str(),
        extended,
    )
    .await
    .expect("extend");

    let live = host
        .get_execute_session(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("live");
    let pc_new = live.mint_plan_commit_ref();
    let artifact_new = minimal_artifact();
    let id_new = crate::operation::compute_plan_commit_id_from_semantic(
        &plan_commit_canonical_comp(&artifact_new.comp),
    );
    register_plan_commit_and_persist(
        &host,
        created.prompt_hash.as_str(),
        created.session.as_str(),
        rehydrate_record(
            &live,
            pc_new.clone(),
            id_new.clone(),
            live.domain_revision,
            live.flow_policy.revision_or_default(),
            artifact_new,
            "after-extend".into(),
            Default::default(),
            PlanDryVerdict::Ok,
            std::time::Instant::now() + PLAN_COMMIT_TTL,
            PlanCommitDryCache::default(),
        ),
    )
    .await
    .expect("pc_new");

    // Drop older plan from hot only; durable still holds both.
    live.remove_plan_commit(&pc0);
    assert!(live.get_plan_commit(&pc0).is_none());
    assert!(live.get_plan_commit(&pc_new).is_some());

    let outcome = host
        .execute_session_registry
        .merge_into_live_session(
            &live,
            created.prompt_hash.as_str(),
            created.session.as_str(),
        )
        .await;
    assert_eq!(outcome, MergeLiveOutcome::Merged);
    verify_plan_commit_id(&live, &pc0, id0).expect("older plan restored by merge");
    verify_plan_commit_id(&live, &pc_new, id_new).expect("newer plan kept");
}

/// Fail-closed replace: durable write runs before hot is advanced (ordering lock).
#[tokio::test]
async fn replace_execute_session_persists_before_hot_advance() {
    let (host, created, live) = open_overshow_profile_host().await;
    let mut next = (*live).clone();
    next.domain_revision = live.domain_revision.saturating_add(1);
    next.prompt_text.push_str("\n# fail-closed wave\n");
    host.replace_execute_session(
        created.prompt_hash.as_str(),
        created.session.as_str(),
        next.clone(),
    )
    .await
    .expect("replace");
    let durable = host
        .execute_session_registry
        .load(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("durable");
    let hot = host
        .get_execute_session(created.prompt_hash.as_str(), created.session.as_str())
        .await
        .expect("hot");
    assert_eq!(durable.domain_revision, next.domain_revision);
    assert_eq!(hot.domain_revision, next.domain_revision);
    assert!(durable.prompt_text.contains("fail-closed wave"));
    assert!(hot.prompt_text.contains("fail-closed wave"));
}
