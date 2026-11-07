//! Experimental identities over the production persistence/ownership seam.
//! No public request contract or production key is changed by these proofs.
use super::*;

fn scope(session: &str, epoch: u64) -> String {
    plasm_core::catalog_discovery::content_hash(
        &serde_json::to_vec(&("objective-epoch-experiment-v1", session, epoch)).unwrap(),
    )
}

fn request(scope: &str, intent: &crate::intent_provenance::IntentProvenance) -> String {
    let allowed =
        DiscoveryAuthorization::catalogs(std::collections::BTreeSet::from(["matrix".into()]));
    let search = search_key("fixture-generation", intent, &allowed, "fixture", &[]).unwrap();
    plasm_core::catalog_discovery::content_hash(&serde_json::to_vec(&(scope, search)).unwrap())
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL"]
async fn objective_epoch_continuation_revision_and_replay() {
    let (a, b) = services().await;
    let session = uuid::Uuid::new_v4().to_string();
    let first = crate::intent_provenance::IntentProvenance::from_turns([
        "Inspect specimens; preserve identifiers.".into(),
    ])
    .unwrap();
    let paraphrase = first
        .derived("Examine specimens without changing identifiers.".into())
        .unwrap();
    let observation = paraphrase
        .derived(
            "Inspect specimens, including the newly observed unit; preserve identifiers.".into(),
        )
        .unwrap();
    let calls = AtomicUsize::new(0);
    // The last row deliberately uses identical ancestry in a new epoch:
    // revisiting a formerly held objective must not reuse its old search completion.
    for (epoch, intent, expected_visits) in [
        (0, &first, 0),
        (0, &paraphrase, 1),
        (0, &observation, 2),
        (1, &first, 0),
    ] {
        let frontier = scope(&session, epoch);
        let key = request(&frontier, intent);
        let result = a
            .route_frontier(&frontier, &key, &[], |visits, _, _| {
                assert_eq!(visits.is_empty(), expected_visits == 0);
                assert!(visits.values().all(|n| *n == expected_visits));
                async {
                    assert!(a
                        .store
                        .searches()
                        .reserve(SearchKey(&key))
                        .await?
                        .is_fresh());
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(receipt())
                }
            })
            .await
            .unwrap();
        let replay = b
            .route_frontier(&frontier, &key, &[], |_, _, _| async {
                panic!("replay must not reserve budget or repeat judgment")
            })
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    for (epoch, count) in [(0, 3), (1, 1)] {
        let saved: SearchFrontier = b
            .store
            .searches()
            .frontier(FrontierKey(&scope(&session, epoch)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.completed.len(), count);
        assert!(saved.visits().values().all(|n| *n == count));
    }
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL"]
async fn objective_epoch_late_completion_and_concurrent_duplicate() {
    let (a, b) = services().await;
    let session = uuid::Uuid::new_v4().to_string();
    let old = scope(&session, 0);
    let new = scope(&session, 1);
    let intent =
        crate::intent_provenance::IntentProvenance::from_turns(["Inspect specimens.".into()])
            .unwrap();
    let old_key = request(&old, &intent);
    let new_key = request(&new, &intent);
    assert_ne!(old_key, new_key);
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let owner = {
        let started = started.clone();
        let release = release.clone();
        let old = old.clone();
        let old_key = old_key.clone();
        tokio::spawn(async move {
            a.route_frontier(&old, &old_key, &[], |_, _, _| async {
                assert!(a
                    .store
                    .searches()
                    .reserve(SearchKey(&old_key))
                    .await?
                    .is_fresh());
                started.notify_one();
                release.notified().await;
                Ok(receipt())
            })
            .await
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    // New objective can complete while the obsolete objective is still running.
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        b.route_frontier(&new, &new_key, &[], |visits, _, _| {
            assert!(visits.is_empty());
            async {
                assert!(b
                    .store
                    .searches()
                    .reserve(SearchKey(&new_key))
                    .await?
                    .is_fresh());
                Ok(receipt())
            }
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let mut duplicate = Box::pin(b.route_frontier(&old, &old_key, &[], |_, _, _| async {
        panic!("concurrent duplicate must not execute work")
    }));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut duplicate)
            .await
            .is_err()
    );
    release.notify_one();
    owner.await.unwrap().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), duplicate)
        .await
        .unwrap()
        .unwrap();
    for frontier in [&old, &new] {
        let saved: SearchFrontier = b
            .store
            .searches()
            .frontier(FrontierKey(frontier))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            saved.completed.len(),
            1,
            "late completion must remain in its own epoch"
        );
    }
}
