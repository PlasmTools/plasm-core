use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::sync::Notify;

fn receipt() -> RoutingReceipt {
    let packet: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../fixtures/discovery/partial-routing.json"
    ))
    .unwrap();
    serde_json::from_value(packet["routing"].clone()).unwrap()
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
    let key = frontier_key("session", "revision", &allowed, "model").unwrap();
    assert_eq!(
        key,
        frontier_key("session", "revision", &allowed, "model").unwrap()
    );
    assert_ne!(
        key,
        frontier_key("other", "revision", &allowed, "model").unwrap()
    );
    assert_ne!(
        key,
        frontier_key("session", "new", &allowed, "model").unwrap()
    );
    assert_ne!(
        key,
        frontier_key("session", "revision", &allowed, "other").unwrap()
    );
    let narrowed = DiscoveryAuthorization::catalogs(Default::default());
    assert_ne!(
        key,
        frontier_key("session", "revision", &narrowed, "model").unwrap()
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
