use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::Notify;

#[path = "objective_epoch_tests.rs"]
mod objective_epoch_tests;

struct AcceptContribution(AtomicUsize);

#[async_trait::async_trait]
impl plasm_runtime::provider_transport::ProviderTransport for AcceptContribution {
    async fn send(
        &self,
        body: &str,
        _: &mut plasm_runtime::provider_transport::AttemptRecorder<'_>,
    ) -> std::result::Result<
        plasm_runtime::provider_transport::ProviderResponse,
        plasm_runtime::provider_transport::ProviderTransportError,
    > {
        self.0.fetch_add(1, Ordering::SeqCst);
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        let answers: std::collections::BTreeMap<_, _> = body["questions"]
            .as_object()
            .unwrap()
            .keys()
            .map(|key| {
                (
                    key.clone(),
                    serde_json::json!({"type":"choice","choice":"relevant","confidence":1.0,
                "probabilities":{"relevant":1.0,"uncertain":0.0,"unrelated":0.0}}),
                )
            })
            .collect();
        Ok(plasm_runtime::provider_transport::ProviderResponse {
            status: reqwest::StatusCode::OK,
            retry_after: None,
            body: serde_json::json!({"model":matcher::RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string(),
        })
    }
}

/// Exercise the real database-to-service boundary, not a preselected matcher fixture.
#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with pgvector"]
async fn hybrid_service_admits_vector_only_candidate() {
    use plasm_core::catalog_discovery::{content_hash, EmbeddingProfile};
    use std::collections::{BTreeMap, BTreeSet};
    let url = std::env::var("PLASM_TEST_POSTGRES_URL").unwrap();
    let store = DiscoveryStore::connect(&url).await.unwrap();
    store.migrate().await.unwrap();
    let catalog = crate::discovery_store::tests::prepared("hybrid-proof", 1);
    let bindings = plasm_core::prerequisites::DeploymentBindings {
        bindings: vec![plasm_core::prerequisites::DeploymentBinding {
            consumer: CapabilityRef {
                catalog: "hybrid-proof".into(),
                capability: "read".into(),
            },
            requirement: "scoped_access".into(),
            provider_catalog: "hybrid-proof".into(),
            provider: "value_source".into(),
        }],
    };
    let generation = store
        .import("hybrid-proof", vec![catalog], &bindings)
        .await
        .unwrap();
    // Isolate the durable contribution cache across repeated proof invocations.
    let intent = format!("xylovariance {}", uuid::Uuid::new_v4());
    let intent = intent.as_str(); // No lexical overlap with the abstract catalogue.
    let profile = EmbeddingProfile::default();
    let key = content_hash(&serde_json::to_vec(&(intent, &profile)).unwrap());
    let mut vector = vec![0.0; 1536];
    vector[0] = 1.0;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("INSERT INTO discovery_intent_embeddings VALUES ($1,$2,$3::text::vector) ON CONFLICT DO NOTHING")
        .bind(key).bind(serde_json::to_value(profile).unwrap()).bind(serde_json::to_string(&vector).unwrap())
        .execute(&pool).await.unwrap();
    pool.close().await;
    let reference = CapabilityRef {
        catalog: "hybrid-proof".into(),
        capability: "create".into(),
    };
    let allowed = DiscoveryAuthorization {
        catalogs: BTreeSet::from([reference.catalog.clone()]),
        capabilities: BTreeMap::from([(
            reference.catalog.clone(),
            BTreeSet::from([reference.capability.clone()]),
        )]),
    };
    let recall = store
        .retrieve(&generation, intent, &allowed, &Default::default())
        .await
        .unwrap();
    assert_eq!(recall.lexical_count, 0);
    assert_eq!(recall.vector_count, 1);
    let mut service = DiscoveryService::cached_test_service(store);
    let transport = Arc::new(AcceptContribution(AtomicUsize::new(0)));
    service.transport = transport.clone();
    let provenance =
        crate::intent_provenance::IntentProvenance::from_turns([intent.into()]).unwrap();
    let key = uuid::Uuid::new_v4().to_string();
    let receipt = service
        .route_cached(&key, &[], || async {
            service
                .route_uncached(
                    &key,
                    &generation,
                    &provenance,
                    &allowed,
                    &[],
                    &BTreeMap::new(),
                )
                .await
        })
        .await
        .unwrap();
    assert_eq!(
        receipt.retrieval.vector_count, 1,
        "service must consume hybrid recall"
    );
    assert_eq!(receipt.retrieval.lexical_count, 0);
    let candidate = receipt
        .retrieval
        .candidates
        .iter()
        .find(|c| c.reference == reference)
        .unwrap();
    assert!(candidate.admissions.contains("vector"));
    assert!(receipt
        .matching
        .matches
        .iter()
        .any(|m| m.capability_id == candidate.id));
    assert_eq!(transport.0.load(Ordering::SeqCst), 1);
    let other = DiscoveryService::cached_test_service(DiscoveryStore::connect(&url).await.unwrap());
    let reused = other
        .route_cached(&key, &[], || async {
            panic!("completed hybrid search must reuse completion across stores")
        })
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(receipt).unwrap(),
        serde_json::to_value(reused).unwrap()
    );
}

fn receipt() -> RoutingReceipt {
    let packet: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../fixtures/discovery/partial-routing.json"
    ))
    .unwrap();
    serde_json::from_value(packet["routing"].clone()).unwrap()
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn self_contained_requests_advance_frontier_but_exact_replays_do_not() {
    let (a, b) = services().await;
    let generation = uuid::Uuid::new_v4().to_string();
    let allowed =
        DiscoveryAuthorization::catalogs(std::collections::BTreeSet::from(["matrix".into()]));
    let first = crate::intent_provenance::IntentProvenance::from_turns([
        "Inspect current specimens without changes.".into(),
    ])
    .unwrap();
    let repeated = first.derived(first.current().into()).unwrap();
    let changed = repeated
        .derived("Calibrate the identified specimen; preserve other specimens.".into())
        .unwrap();
    let calls = AtomicUsize::new(0);
    let mut previous = None;
    for (intent, expected_visits) in [(&first, 0), (&repeated, 1), (&changed, 0)] {
        let scope = frontier_key(
            "session",
            &generation,
            &allowed,
            "fixture",
            intent.current(),
        )
        .unwrap();
        let key = search_key(&generation, intent, &allowed, "fixture", &[]).unwrap();
        assert_ne!(previous.as_ref(), Some(&key));
        let calls = &calls;
        let reject = intent.current() == first.current();
        let actual = a
            .route_frontier(&scope, &key, &[], |visits| async move {
                assert!(visits.values().all(|count| *count == expected_visits));
                assert_eq!(visits.is_empty(), expected_visits == 0);
                calls.fetch_add(1, Ordering::SeqCst);
                let mut result = receipt();
                if reject {
                    for matched in &mut result.matching.matches {
                        matched.choice = matcher::MatchChoice::Unrelated;
                    }
                }
                Ok(result)
            })
            .await
            .unwrap();
        let replay = b
            .route_frontier(&scope, &key, &[], |_| async {
                panic!("exact completed work must be reused across stores")
            })
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
        previous = Some(key);
    }
    let recreated = DiscoveryService::cached_test_service(
        DiscoveryStore::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
            .await
            .unwrap(),
    );
    let scope = frontier_key(
        "session",
        &generation,
        &allowed,
        "fixture",
        changed.current(),
    )
    .unwrap();
    recreated
        .route_frontier(&scope, previous.as_ref().unwrap(), &[], |_| async {
            panic!("reconnecting must not replay completed work")
        })
        .await
        .unwrap();
    let saved: SearchFrontier = serde_json::from_str(
        &recreated
            .store
            .cached_selector_envelope(&scope)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(saved.completed.len(), 1);
    assert!(saved.visits().values().all(|count| *count == 1));
    let old_scope =
        frontier_key("session", &generation, &allowed, "fixture", first.current()).unwrap();
    let old_key = search_key(&generation, &first, &allowed, "fixture", &[]).unwrap();
    recreated
        .route_frontier(&old_scope, &old_key, &[], |_| async {
            panic!("returning to a prior request must reuse its completion")
        })
        .await
        .unwrap();
    let old: SearchFrontier = serde_json::from_str(
        &recreated
            .store
            .cached_selector_envelope(&old_scope)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(old.completed.len(), 2);
    assert!(old.visits().values().all(|count| *count == 2));
    let returning = changed.derived(first.current().into()).unwrap();
    let returning_key = search_key(&generation, &returning, &allowed, "fixture", &[]).unwrap();
    recreated
        .route_frontier(&old_scope, &returning_key, &[], |visits| async move {
            assert!(!visits.is_empty());
            assert!(
                visits.values().all(|count| *count == 2),
                "returning to inspection must restore exploration progress"
            );
            Ok(receipt())
        })
        .await
        .unwrap();
    let exposed = vec![CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    }];
    assert_ne!(
        previous.unwrap(),
        search_key(&generation, &changed, &allowed, "fixture", &exposed).unwrap()
    );
}

async fn services() -> (Arc<DiscoveryService>, Arc<DiscoveryService>) {
    let url = std::env::var("PLASM_TEST_POSTGRES_URL").unwrap();
    let a = DiscoveryStore::connect(&url).await.unwrap();
    a.store_selector_envelope("test-schema-check", "{}")
        .await
        .unwrap();
    let b = DiscoveryStore::connect(&url).await.unwrap();
    (
        Arc::new(DiscoveryService::cached_test_service(a)),
        Arc::new(DiscoveryService::cached_test_service(b)),
    )
}

#[test]
fn frontier_identity_separates_sessions_revisions_models_and_authorization() {
    let allowed =
        DiscoveryAuthorization::catalogs(std::collections::BTreeSet::from(["matrix".into()]));
    let key = frontier_key("session", "revision", &allowed, "model", "Inspect").unwrap();
    assert_ne!(
        key,
        frontier_key(
            "session",
            "revision",
            &allowed,
            "model",
            "Calibrate; preserve originals"
        )
        .unwrap(),
        "a rejection for inspection must not penalize a new calibration request"
    );
    assert_eq!(
        key,
        frontier_key("session", "revision", &allowed, "model", "Inspect").unwrap()
    );
    assert_ne!(
        key,
        frontier_key("other", "revision", &allowed, "model", "Inspect").unwrap()
    );
    assert_ne!(
        key,
        frontier_key("session", "new", &allowed, "model", "Inspect").unwrap()
    );
    assert_ne!(
        key,
        frontier_key("session", "revision", &allowed, "other", "Inspect").unwrap()
    );
    let narrowed = DiscoveryAuthorization::catalogs(Default::default());
    assert_ne!(
        key,
        frontier_key("session", "revision", &narrowed, "model", "Inspect").unwrap()
    );
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn frontier_reuses_completion_once_and_survives_store_recreation() {
    let (a, b) = services().await;
    let scope = uuid::Uuid::new_v4().to_string();
    let first = format!("{scope}:first");
    let second = format!("{scope}:second");
    let expected = receipt();
    a.route_frontier(&scope, &first, &[], |visits| async move {
        assert!(visits.is_empty());
        Ok(receipt())
    })
    .await
    .unwrap();
    // Receipt persistence succeeded but the frontier checkpoint did not.
    // The saved completion must repair progress without repeating judgment.
    b.store
        .store_search_progress(
            &scope,
            &serde_json::to_string(&SearchFrontier::default()).unwrap(),
        )
        .await
        .unwrap();
    b.route_frontier(&scope, &first, &[], |_| async {
        panic!("duplicate request must not spend work or advance again")
    })
    .await
    .unwrap();
    let mut expected_frontier = SearchFrontier::default();
    expected_frontier.record(&first, &expected);
    let expected_visits = expected_frontier.visits();
    assert!(!expected_visits.is_empty());
    b.route_frontier(&scope, &second, &[], |visits| async move {
        assert_eq!(visits, expected_visits);
        Ok(receipt())
    })
    .await
    .unwrap();
    let saved: SearchFrontier = serde_json::from_str(
        &a.store
            .cached_selector_envelope(&scope)
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(saved.completed.len(), 2);
    assert!(saved.visits().values().all(|count| *count == 2));
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn concurrent_frontier_turns_observe_prior_completion_without_double_spending() {
    let (a, b) = services().await;
    let scope = uuid::Uuid::new_v4().to_string();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let owner = {
        let scope = scope.clone();
        let started = started.clone();
        let release = release.clone();
        tokio::spawn(async move {
            a.route_frontier(&scope, &format!("{scope}:first"), &[], |_| async {
                started.notify_one();
                release.notified().await;
                Ok(receipt())
            })
            .await
        })
    };
    started.notified().await;
    let next = format!("{scope}:next");
    let mut follower = Box::pin(b.route_frontier(&scope, &next, &[], |visits| async move {
        assert!(!visits.is_empty());
        assert!(visits.values().all(|count| *count == 1));
        Ok(receipt())
    }));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut follower)
            .await
            .is_err()
    );
    release.notify_one();
    owner.await.unwrap().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), follower)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn concurrent_search_waits_for_other_store_and_reuses_completion() {
    let (a, b) = services().await;
    let key = uuid::Uuid::new_v4().to_string();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let calls = Arc::new(AtomicUsize::new(0));
    let owner = {
        let key = key.clone();
        let started = started.clone();
        let release = release.clone();
        let calls = calls.clone();
        tokio::spawn(async move {
            a.route_cached(&key, &[], || async {
                assert!(
                    a.store
                        .reserve_selector_search(&format!("{key}:started"))
                        .await?
                );
                calls.fetch_add(1, Ordering::SeqCst);
                started.notify_one();
                release.notified().await;
                Ok(receipt())
            })
            .await
        })
    };
    started.notified().await;
    let mut follower = Box::pin(b.route_cached(&key, &[], || async {
        if !b
            .store
            .reserve_selector_search(&format!("{key}:started"))
            .await?
        {
            return Err(DiscoveryServiceError::SearchAlreadyStarted);
        }
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(receipt())
    }));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut follower)
            .await
            .is_err(),
        "live owner must be awaited, not reported as interrupted"
    );
    release.notify_one();
    let expected = owner.await.unwrap().unwrap();
    let actual = tokio::time::timeout(std::time::Duration::from_secs(5), follower)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(expected).unwrap(),
        serde_json::to_value(actual).unwrap()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn cancelled_or_failed_owner_releases_waiters_without_replaying_budget() {
    for cancel in [false, true] {
        let (a, b) = services().await;
        let key = uuid::Uuid::new_v4().to_string();
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let owner = {
            let key = key.clone();
            let started = started.clone();
            let release = release.clone();
            tokio::spawn(async move {
                a.route_frontier(&format!("{key}:frontier"), &key, &[], |visits| {
                    assert!(visits.is_empty());
                    async {
                        assert!(
                            a.store
                                .reserve_selector_search(&format!("{key}:started"))
                                .await?
                        );
                        a.store
                            .store_search_progress(&format!("{key}:progress"), "{\"requests\":1}")
                            .await?;
                        started.notify_one();
                        release.notified().await;
                        Err(DiscoveryServiceError::SearchAlreadyStarted)
                    }
                })
                .await
            })
        };
        started.notified().await;
        let scope = format!("{key}:frontier");
        let mut follower = Box::pin(b.route_frontier(&scope, &key, &[], |visits| {
            assert!(visits.is_empty());
            async {
                assert!(
                    !b.store
                        .reserve_selector_search(&format!("{key}:started"))
                        .await?
                );
                assert_eq!(
                    b.store
                        .cached_selector_envelope(&format!("{key}:progress"))
                        .await?,
                    Some("{\"requests\":1}".into())
                );
                Err(DiscoveryServiceError::SearchAlreadyStarted)
            }
        }));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut follower)
                .await
                .is_err()
        );
        if cancel {
            owner.abort();
        } else {
            release.notify_one();
        }
        let _ = owner.await;
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(5), follower)
                .await
                .unwrap(),
            Err(DiscoveryServiceError::SearchAlreadyStarted)
        ));
    }
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn cancelled_waiter_does_not_cancel_owner_or_block_distinct_searches() {
    let (a, b) = services().await;
    let key = uuid::Uuid::new_v4().to_string();
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let owner = {
        let key = key.clone();
        let started = started.clone();
        let release = release.clone();
        tokio::spawn(async move {
            a.route_cached(&key, &[], || async {
                assert!(
                    a.store
                        .reserve_selector_search(&format!("{key}:started"))
                        .await?
                );
                started.notify_one();
                release.notified().await;
                Ok(receipt())
            })
            .await
        })
    };
    started.notified().await;
    let waiter = b.route_cached(&key, &[], || async { panic!("waiter must not run work") });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), waiter)
            .await
            .is_err()
    );
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        b.route_cached(&uuid::Uuid::new_v4().to_string(), &[], || async {
            Ok(receipt())
        }),
    )
    .await
    .unwrap()
    .unwrap();
    release.notify_one();
    owner.await.unwrap().unwrap();
    b.route_cached(&key, &[], || async {
        panic!("completed work must be cached")
    })
    .await
    .unwrap();
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn distinct_searches_leave_pool_capacity_for_progress_and_completion() {
    let (a, _) = services().await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..24 {
        let a = a.clone();
        tasks.spawn(async move {
            let key = uuid::Uuid::new_v4().to_string();
            a.route_cached(&key, &[], || async {
                assert!(
                    a.store
                        .reserve_selector_search(&format!("{key}:started"))
                        .await?
                );
                tokio::task::yield_now().await;
                a.store
                    .store_search_progress(&format!("{key}:progress"), "{}")
                    .await?;
                Ok(receipt())
            })
            .await
        });
    }
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(result) = tasks.join_next().await {
            result.unwrap().unwrap();
        }
    })
    .await
    .expect("search ownership must not exhaust connections needed by its work");
}
