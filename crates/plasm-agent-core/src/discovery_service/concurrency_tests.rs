use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;

fn progress(requests: usize) -> matcher::online::SearchProgress {
    let mut accounting = matcher::online::SearchReceipt::default();
    accounting.requests = requests;
    serde_json::from_value(serde_json::json!({
        "state": {"state": "admitted"},
        "accounting": accounting,
        "judgments": [],
        "pending": [],
        "paths": []
    }))
    .unwrap()
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn typed_search_repository_preserves_record_keys_and_write_contracts() {
    let (a, b) = services().await;
    let key = uuid::Uuid::new_v4().to_string();
    let repo = a.store.searches();
    assert!(matches!(
        repo.reserve(SearchKey(&key)).await.unwrap(),
        SearchAdmission::Fresh
    ));
    assert!(matches!(
        b.store.searches().reserve(SearchKey(&key)).await.unwrap(),
        SearchAdmission::PreviouslyStarted(None)
    ));
    repo.checkpoint(SearchKey(&key), &progress(3))
        .await
        .unwrap();
    let SearchAdmission::PreviouslyStarted(Some(saved)) =
        b.store.searches().reserve(SearchKey(&key)).await.unwrap()
    else {
        panic!("spent search must return its persisted progress")
    };
    assert_eq!(saved.accounting().requests, 3);
    repo.record_decision(&format!("{key}-decision"), "first")
        .await
        .unwrap();
    repo.record_decision(&format!("{key}-decision"), "second")
        .await
        .unwrap();
    assert_eq!(
        b.store
            .searches()
            .decision(&format!("{key}-decision"))
            .await
            .unwrap()
            .as_deref(),
        Some("first")
    );
    let receipt = receipt();
    repo.complete(SearchKey(&key), &receipt).await.unwrap();
    let original = serde_json::to_value(&receipt).unwrap();
    let mut other = receipt;
    other.pin_id = "different".into();
    repo.complete(SearchKey(&key), &other).await.unwrap();
    let saved = b
        .store
        .searches()
        .completion(SearchKey(&key))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(saved).unwrap(), original);
    let pool = sqlx::PgPool::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
        .await
        .unwrap();
    let marker: String =
        sqlx::query_scalar("SELECT envelope FROM discovery_selector_cache WHERE cache_key=$1")
            .bind(format!("{key}:started"))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(marker, "{\"started\":true}");
    let raw: String =
        sqlx::query_scalar("SELECT envelope FROM discovery_selector_cache WHERE cache_key=$1")
            .bind(format!("{key}:progress"))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&raw).unwrap(),
        serde_json::to_value(progress(3)).unwrap()
    );
    sqlx::query("UPDATE discovery_selector_cache SET envelope=$1 WHERE cache_key=$2")
        .bind("{invalid")
        .bind(format!("{key}:progress"))
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        matches!(
            repo.reserve(SearchKey(&key)).await,
            Err(crate::discovery_store::DiscoveryStoreError::Json(_))
        ),
        "corrupt progress must not restore a fresh budget"
    );
    pool.close().await;
}

/// Spawned by the companion test; ordinary ignored-suite runs leave it inert.
#[tokio::test]
#[ignore = "subprocess helper; requires an isolated PLASM_TEST_POSTGRES_URL"]
async fn subprocess_search_lock_owner() {
    let Ok(key) = std::env::var("PLASM_DISCOVERY_TEST_CHILD_KEY") else {
        return;
    };
    let url = std::env::var("PLASM_TEST_POSTGRES_URL").unwrap();
    let store = DiscoveryStore::connect(&url).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    store
        .searches()
        .with_search(SearchKey(&key), || async {
            assert!(matches!(
                store.searches().reserve(SearchKey(&key)).await.unwrap(),
                SearchAdmission::Fresh
            ));
            sqlx::query(
                "INSERT INTO discovery_selector_cache (cache_key,envelope) VALUES ($1,'ready')",
            )
            .bind(format!("{key}-ready"))
            .execute(&pool)
            .await
            .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let released: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM discovery_selector_cache WHERE cache_key=$1)",
                    )
                    .bind(format!("{key}-release"))
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                    if released {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("parent must release the child");
        })
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL and subprocess execution"]
async fn separate_process_waits_for_search_owner_and_preserves_spent_admission() {
    let (service, _) = services().await;
    let key = uuid::Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--ignored")
        .arg("--exact")
        .arg("discovery_service::concurrency_tests::subprocess_search_lock_owner")
        .env("PLASM_DISCOVERY_TEST_CHILD_KEY", &key)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let pool = sqlx::PgPool::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let ready: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM discovery_selector_cache WHERE cache_key=$1)",
            )
            .bind(format!("{key}-ready"))
            .fetch_one(&pool)
            .await
            .unwrap();
            if ready {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("child must hold the database search lock");
    let mut follower = Box::pin(service.route_cached(&key, &[], || async {
        match service.store.searches().reserve(SearchKey(&key)).await? {
            SearchAdmission::PreviouslyStarted(None) => {
                Err(DiscoveryServiceError::SearchAlreadyStarted)
            }
            _ => panic!("a second process must not receive a fresh budget"),
        }
    }));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(150), &mut follower)
            .await
            .is_err(),
        "a different process must wait for the owner"
    );
    sqlx::query("INSERT INTO discovery_selector_cache (cache_key,envelope) VALUES ($1,'release')")
        .bind(format!("{key}-release"))
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(10), follower)
            .await
            .unwrap(),
        Err(DiscoveryServiceError::SearchAlreadyStarted)
    ));
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("child must exit after release");
    assert!(status.success());
    pool.close().await;
}

#[path = "objective_epoch_tests.rs"]
mod objective_epoch_tests;

struct AcceptContribution(AtomicUsize);

struct AcceptBusinessRead;

fn add_contextual_probability(
    request: &serde_json::Value,
    answers: &mut serde_json::Map<String, serde_json::Value>,
) {
    for (key, answer) in answers.iter_mut() {
        if request["questions"][key]["criteria"]
            .get("contextual")
            .is_some()
        {
            answer["probabilities"]["contextual"] = serde_json::json!(0.0);
        }
    }
}

#[async_trait::async_trait]
impl plasm_runtime::provider_transport::ProviderTransport for AcceptBusinessRead {
    async fn send(
        &self,
        body: &str,
        _: &mut plasm_runtime::provider_transport::AttemptRecorder<'_>,
    ) -> std::result::Result<
        plasm_runtime::provider_transport::ProviderResponse,
        plasm_runtime::provider_transport::ProviderTransportError,
    > {
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        let mut answers: serde_json::Map<String, serde_json::Value> = body["questions"].as_object().unwrap().iter().map(|(key, question)| {
            let instruction = question["instructions"].as_str().unwrap();
            let witness = instruction.split("question_evidence.").nth(1).unwrap();
            let (scope, tail) = witness.split_once(".capabilities.").unwrap();
            let target = tail.split('`').next().unwrap();
            let card = &body["state"]["question_evidence"][scope]["capabilities"][target];
            let relevant = card.to_string().contains("Read business records");
            let (choice, yes, no) = if relevant { ("relevant", 1.0, 0.0) } else { ("unrelated", 0.0, 1.0) };
            (key.clone(), serde_json::json!({"type":"choice","choice":choice,"confidence":1.0,"probabilities":{"relevant":yes,"unrelated":no,"uncertain":0.0}}))
        }).collect();
        add_contextual_probability(&body, &mut answers);
        Ok(plasm_runtime::provider_transport::ProviderResponse { status: reqwest::StatusCode::OK, retry_after: None, body: serde_json::json!({"model":matcher::RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string() })
    }
}

struct DelayedContribution {
    packets: std::sync::Mutex<Vec<String>>,
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn decision_fill_ownership_is_keyed_and_released_on_cancellation() {
    let (a, b) = services().await;
    let key = uuid::Uuid::new_v4().to_string();
    let other = format!("{key}-other");
    let first = a.store.searches().claim_decision(&key).await.unwrap();
    let distinct = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        a.store.searches().claim_decision(&other),
    )
    .await
    .unwrap()
    .unwrap();
    let repo = b.store.searches();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            repo.claim_decision(&key)
        )
        .await
        .is_err()
    );
    let spare_a = format!("{key}-spare-a");
    let spare_b = format!("{key}-spare-b");
    let spare_a = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        repo.claim_decision(&spare_a),
    )
    .await
    .unwrap()
    .unwrap();
    let spare_b = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        repo.claim_decision(&spare_b),
    )
    .await
    .expect("cancelled waiter must release its admission promptly")
    .unwrap();
    drop((spare_a, spare_b));
    let waiting = repo.claim_decision(&key);
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut waiting)
            .await
            .is_err()
    );
    assert!(b.store.searches().decision(&key).await.unwrap().is_none());
    drop(first);
    let next = tokio::time::timeout(std::time::Duration::from_secs(2), &mut waiting)
        .await
        .unwrap()
        .unwrap();
    drop(next);
    drop(distinct);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        a.store.searches().claim_decision(&key),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
#[ignore = "subprocess helper; requires an isolated PLASM_TEST_POSTGRES_URL"]
async fn subprocess_decision_fill_owner() {
    let Ok(key) = std::env::var("PLASM_DISCOVERY_TEST_DECISION_KEY") else {
        return;
    };
    let url = std::env::var("PLASM_TEST_POSTGRES_URL").unwrap();
    let store = DiscoveryStore::connect(&url).await.unwrap();
    let _owner = store.searches().claim_decision(&key).await.unwrap();
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("INSERT INTO discovery_selector_cache (cache_key,envelope) VALUES ($1,'ready')")
        .bind(format!("{key}-ready"))
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL and subprocess execution"]
async fn decision_fill_owner_is_released_when_another_process_dies() {
    let (service, _) = services().await;
    let key = uuid::Uuid::new_v4().to_string();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--ignored")
        .arg("--exact")
        .arg("discovery_service::concurrency_tests::subprocess_decision_fill_owner")
        .env("PLASM_DISCOVERY_TEST_DECISION_KEY", &key)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let pool = sqlx::PgPool::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let ready: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM discovery_selector_cache WHERE cache_key=$1)",
            )
            .bind(format!("{key}-ready"))
            .fetch_one(&pool)
            .await
            .unwrap();
            if ready {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("child must acquire decision ownership");
    let repo = service.store.searches();
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(150),
            repo.claim_decision(&key)
        )
        .await
        .is_err()
    );
    child.kill().unwrap();
    child.wait().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), repo.claim_decision(&key))
        .await
        .unwrap()
        .unwrap();
    pool.close().await;
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with permission to terminate its own backend"]
async fn lost_decision_fill_owner_cannot_publish_a_stale_response() {
    let (service, _) = services().await;
    let key = uuid::Uuid::new_v4().to_string();
    let mut stale = service.store.searches().claim_decision(&key).await.unwrap();
    let pid = stale.backend_pid().await.unwrap();
    let pool = sqlx::PgPool::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
        .await
        .unwrap();
    let killed: bool = sqlx::query_scalar("SELECT pg_terminate_backend($1)")
        .bind(pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(killed);
    assert!(stale.persist(&key, "stale").await.is_err());
    drop(stale);
    assert!(
        service
            .store
            .searches()
            .decision(&key)
            .await
            .unwrap()
            .is_none()
    );
    let mut owner = service.store.searches().claim_decision(&key).await.unwrap();
    owner.persist(&key, "canonical").await.unwrap();
    assert_eq!(
        service
            .store
            .searches()
            .decision(&key)
            .await
            .unwrap()
            .as_deref(),
        Some("canonical")
    );
    pool.close().await;
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn full_frontier_and_decision_admission_leaves_room_for_persistence() {
    let (service, _) = services().await;
    let entered = Arc::new(tokio::sync::Barrier::new(5));
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..4 {
        let service = service.clone();
        let entered = entered.clone();
        let release = release.clone();
        tasks.spawn(async move {
            let key = uuid::Uuid::new_v4().to_string();
            service
                .store
                .searches()
                .with_frontier(FrontierKey(&key), || async {
                    entered.wait().await;
                    let _permit = release.acquire().await.unwrap();
                })
                .await
                .unwrap();
        });
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.wait())
        .await
        .unwrap();
    let first_key = uuid::Uuid::new_v4().to_string();
    let second_key = uuid::Uuid::new_v4().to_string();
    let mut first = service
        .store
        .searches()
        .claim_decision(&first_key)
        .await
        .unwrap();
    let second = service
        .store
        .searches()
        .claim_decision(&second_key)
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        first.persist(&first_key, "validated"),
    )
    .await
    .expect("checkpoint needs a free pooled connection")
    .unwrap();
    assert_eq!(
        service
            .store
            .searches()
            .decision(&first_key)
            .await
            .unwrap()
            .as_deref(),
        Some("validated")
    );
    drop((first, second));
    release.add_permits(4);
    while let Some(task) = tasks.join_next().await {
        task.unwrap();
    }
}

#[async_trait::async_trait]
impl plasm_runtime::provider_transport::ProviderTransport for DelayedContribution {
    async fn send(
        &self,
        body: &str,
        _: &mut plasm_runtime::provider_transport::AttemptRecorder<'_>,
    ) -> std::result::Result<
        plasm_runtime::provider_transport::ProviderResponse,
        plasm_runtime::provider_transport::ProviderTransportError,
    > {
        self.packets.lock().unwrap().push(body.to_owned());
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        let request: serde_json::Value = serde_json::from_str(body).unwrap();
        let mut answers: serde_json::Map<_, _> = request["questions"].as_object().unwrap().keys().map(|key| (key.clone(), serde_json::json!({"type":"choice","choice":"relevant","confidence":1.0,"probabilities":{"relevant":1.0,"uncertain":0.0,"unrelated":0.0}}))).collect();
        add_contextual_probability(&request, &mut answers);
        Ok(plasm_runtime::provider_transport::ProviderResponse { status: reqwest::StatusCode::OK, retry_after: None, body: serde_json::json!({"model":matcher::RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string() })
    }
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with pgvector"]
async fn ordinary_and_supplemental_frontiers_share_one_decision_fill() {
    use plasm_core::catalog_discovery::{EmbeddingProfile, content_hash};
    use std::collections::{BTreeMap, BTreeSet};
    let url = std::env::var("PLASM_TEST_POSTGRES_URL").unwrap();
    let first = DiscoveryStore::connect(&url).await.unwrap();
    first.migrate().await.unwrap();
    let second = DiscoveryStore::connect(&url).await.unwrap();
    let catalog = crate::discovery_store::tests::prepared("overlap-proof", 1);
    let bindings = plasm_core::prerequisites::DeploymentBindings {
        bindings: vec![plasm_core::prerequisites::DeploymentBinding {
            consumer: CapabilityRef {
                catalog: "overlap-proof".into(),
                capability: "read".into(),
            },
            requirement: "scoped_access".into(),
            provider_catalog: "overlap-proof".into(),
            provider: "value_source".into(),
        }],
    };
    let generation = first
        .import("overlap-proof", vec![catalog], &bindings)
        .await
        .unwrap();
    let reference = CapabilityRef {
        catalog: "overlap-proof".into(),
        capability: "create".into(),
    };
    let allowed = DiscoveryAuthorization {
        catalogs: BTreeSet::from([reference.catalog.clone()]),
        capabilities: BTreeMap::from([(
            reference.catalog.clone(),
            BTreeSet::from([reference.capability.clone()]),
        )]),
    };
    let intent = format!("xylovariance {}", uuid::Uuid::new_v4());
    let profile = EmbeddingProfile::default();
    let embedding_key = content_hash(&serde_json::to_vec(&(intent.as_str(), &profile)).unwrap());
    let mut vector = vec![0.0; 1536];
    vector[0] = 1.0;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("INSERT INTO discovery_intent_embeddings VALUES ($1,$2,$3::text::vector)")
        .bind(embedding_key)
        .bind(serde_json::to_value(profile).unwrap())
        .bind(serde_json::to_string(&vector).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let session = uuid::Uuid::new_v4().to_string();
    first
        .pin_generation(
            &session,
            &generation,
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .unwrap();
    let provider = Arc::new(DelayedContribution {
        packets: std::sync::Mutex::new(Vec::new()),
    });
    let mut ordinary = DiscoveryService::cached_test_service(first);

    ordinary.transport = provider.clone();
    let mut supplemental = DiscoveryService::cached_test_service(second).supplemental([reference]);

    supplemental.transport = provider.clone();
    let provenance = crate::intent_provenance::IntentProvenance::from_turns([intent]).unwrap();
    ordinary
        .store
        .commit_discovery_progress(&session, &provenance, false)
        .await
        .unwrap();
    let turn = RouteTurn {
        new_generation: &generation,
        intent_provenance: &provenance,
        logical_session: Some(&session),
        allowed: &allowed,
        exposed: &[],
        expires_at: std::time::SystemTime::now() + std::time::Duration::from_secs(3600),
    };
    let (a, b) = tokio::join!(
        ordinary.route_turn(turn),
        supplemental.route_supplemental_turn(
            &session,
            uuid::Uuid::new_v4(),
            &generation,
            &provenance,
            &allowed,
            &[]
        )
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.search.exposed, b.search.exposed);
    let packets = provider.packets.lock().unwrap();
    if packets.len() > 1 {
        assert_eq!(
            packets[0], packets[1],
            "the two actual frontiers must issue an identical decision packet"
        );
    }
    assert_eq!(
        packets.len(),
        1,
        "concurrent frontiers must share one durable fill"
    );
}

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
            .iter()
            .map(|(key, question)| {
                let mut answer = serde_json::json!({"type":"choice","choice":"relevant","confidence":1.0,
                    "probabilities":{"relevant":1.0,"uncertain":0.0,"unrelated":0.0}});
                if question["criteria"].get("contextual").is_some() { answer["probabilities"]["contextual"] = serde_json::json!(0.0); }
                (key.clone(), answer)
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
    use plasm_core::catalog_discovery::{EmbeddingProfile, content_hash};
    use std::collections::{BTreeMap, BTreeSet};
    if std::env::var_os("PLASM_DISCOVERY_OTEL_TEST").is_some() {
        crate::init_embedded_metrics().unwrap();
    }
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
                .route_uncached(RouteWork {
                    key: &key,
                    session: &key,
                    generation: &generation,
                    provenance: &provenance,
                    allowed: &allowed,
                    exposed: &[],
                    visits: &BTreeMap::new(),
                    deferred_priorities: &BTreeMap::new(),
                    operation_state: OperationFrontier::default(),
                })
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
    assert!(
        receipt
            .matching
            .matches
            .iter()
            .any(|m| m.capability_id == candidate.id)
    );
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
    if std::env::var_os("PLASM_DISCOVERY_OTEL_TEST").is_some() {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with pgvector"]
async fn operation_selection_accounts_for_required_acquisitions() {
    use plasm_core::catalog_discovery::{EmbeddingProfile, content_hash};
    use std::collections::{BTreeMap, BTreeSet};
    let url = std::env::var("PLASM_TEST_POSTGRES_URL").unwrap();
    let store = DiscoveryStore::connect(&url).await.unwrap();
    store.migrate().await.unwrap();
    let catalog = crate::discovery_store::tests::prepared("operation-proof", 1);
    let read = CapabilityRef {
        catalog: "operation-proof".into(),
        capability: "read".into(),
    };
    let source = CapabilityRef {
        catalog: "operation-proof".into(),
        capability: "acquire".into(),
    };
    let bindings = plasm_core::prerequisites::DeploymentBindings {
        bindings: vec![plasm_core::prerequisites::DeploymentBinding {
            consumer: read.clone(),
            requirement: "scoped_access".into(),
            provider_catalog: source.catalog.clone(),
            provider: "value_source".into(),
        }],
    };
    let generation = store
        .import("operation-proof", vec![catalog], &bindings)
        .await
        .unwrap();
    let intent = format!("xylovariance {}", uuid::Uuid::new_v4());
    let profile = EmbeddingProfile::default();
    let key = content_hash(&serde_json::to_vec(&(intent.as_str(), &profile)).unwrap());
    let mut vector = vec![0.0; 1536];
    vector[1] = 1.0;
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    sqlx::query("INSERT INTO discovery_intent_embeddings VALUES ($1,$2,$3::text::vector) ON CONFLICT DO NOTHING")
        .bind(key).bind(serde_json::to_value(profile).unwrap()).bind(serde_json::to_string(&vector).unwrap()).execute(&pool).await.unwrap();
    pool.close().await;
    let allowed = DiscoveryAuthorization {
        catalogs: BTreeSet::from([read.catalog.clone()]),
        capabilities: BTreeMap::from([(
            read.catalog.clone(),
            BTreeSet::from([read.capability.clone(), source.capability.clone()]),
        )]),
    };
    let mut service = DiscoveryService::cached_test_service(store);

    service.transport = Arc::new(AcceptBusinessRead);
    let provenance = crate::intent_provenance::IntentProvenance::from_turns([intent]).unwrap();
    let request_key = uuid::Uuid::new_v4().to_string();
    let receipt = service
        .route_uncached(RouteWork {
            key: &request_key,
            session: &request_key,
            generation: &generation,
            provenance: &provenance,
            allowed: &allowed,
            exposed: &[],
            visits: &BTreeMap::new(),
            deferred_priorities: &BTreeMap::new(),
            operation_state: OperationFrontier::default(),
        })
        .await
        .unwrap();
    let closure = receipt.closure.expect("relevant read must have a closure");
    assert!(closure.prerequisites.contains(&source));
    assert!(
        closure
            .prerequisites
            .iter()
            .all(|reference| receipt.search.considered.contains(reference)
                && !receipt.search.unexplored.contains(reference))
    );
}

fn receipt() -> RoutingReceipt {
    let packet: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../fixtures/discovery/partial-routing.json"
    ))
    .unwrap();
    serde_json::from_value(packet["routing"].clone()).unwrap()
}

#[test]
fn deferred_backlog_is_scoped_idempotent_and_never_grants_relevance() {
    use std::collections::BTreeSet;
    let mut receipt = receipt();
    receipt.authorization = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let key =
        deferred_backlog_key("session", "generation", &receipt.authorization, "model").unwrap();
    assert_ne!(
        key,
        deferred_backlog_key("other", "generation", &receipt.authorization, "model").unwrap()
    );
    assert_ne!(
        key,
        deferred_backlog_key("session", "other", &receipt.authorization, "model").unwrap()
    );
    receipt.search.exposed.clear();
    let deferred = CapabilityRef {
        catalog: "matrix".into(),
        capability: "deferred".into(),
    };
    let denied = CapabilityRef {
        catalog: "other".into(),
        capability: "denied".into(),
    };
    receipt.search.deferred = vec![
        matcher::online::DeferredCandidate {
            reference: deferred.clone(),
            reason: matcher::online::DeferredReason::InputBytes,
        },
        matcher::online::DeferredCandidate {
            reference: denied,
            reason: matcher::online::DeferredReason::InputBytes,
        },
    ];
    let mut backlog = DeferredBacklog::default();
    backlog.observe(1, "old".into(), &receipt, "receipt-one".into());
    assert!(backlog.priorities("old", &receipt.authorization).is_empty());
    assert_eq!(
        backlog
            .priorities("new", &receipt.authorization)
            .get(&deferred),
        Some(&1)
    );
    backlog.observe(1, "old".into(), &receipt, "receipt-one".into());
    assert_eq!(backlog.entries[0].age, 1);
    backlog.observe(2, "new".into(), &receipt, "receipt-two".into());
    assert_eq!(backlog.entries[0].age, 2);
    backlog.observe(1, "old".into(), &receipt, "late".into());
    assert_eq!(backlog.last_intent, "new");
    assert_eq!(backlog.entries[0].age, 2);
    assert!(
        backlog
            .priorities(
                "another",
                &DiscoveryAuthorization::catalogs(BTreeSet::new())
            )
            .is_empty()
    );
    assert!(
        receipt
            .matching
            .matches
            .iter()
            .all(|judgment| judgment.capability_id != "deferred")
    );
}

#[test]
fn partial_operation_answers_advance_fairness_without_becoming_judgments() {
    use std::collections::BTreeSet;
    let mut receipt = receipt();
    receipt.authorization = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    receipt.matching.matches.clear();
    let pending = CapabilityRef {
        catalog: "matrix".into(),
        capability: "partially_judged".into(),
    };
    let denied = CapabilityRef {
        catalog: "other".into(),
        capability: "unrelated".into(),
    };
    let mut frontier = SearchFrontier::default();
    frontier.record(
        "first",
        &receipt,
        &BTreeSet::from([pending.clone(), denied.clone()]),
    );
    assert_eq!(frontier.visits().get(&pending), Some(&1));
    assert!(!frontier.visits().contains_key(&denied));
    frontier.record("first", &receipt, &BTreeSet::from([pending.clone()]));
    assert_eq!(frontier.visits().get(&pending), Some(&1));
    assert!(receipt.matching.matches.is_empty());
}

#[test]
fn supplemental_frontier_tracks_judged_candidates_once_and_is_scoped() {
    use std::collections::BTreeSet;
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let narrowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["other".into()]));
    let key = supplemental_frontier_key("session-a", "generation", &allowed, "model").unwrap();
    assert_ne!(
        key,
        supplemental_frontier_key("session-b", "generation", &allowed, "model").unwrap()
    );
    assert_ne!(
        key,
        supplemental_frontier_key("session-a", "other-generation", &allowed, "model").unwrap()
    );
    assert_ne!(
        key,
        supplemental_frontier_key("session-a", "generation", &narrowed, "model").unwrap()
    );
    let mut frontier = SearchFrontier::default();
    let mut first = receipt();
    for matched in &mut first.matching.matches {
        matched.choice = matcher::MatchChoice::Unrelated;
    }
    frontier.record("first", &first, &Default::default());
    assert!(!frontier.visits().is_empty());
    frontier.record("first", &first, &Default::default());
    assert!(frontier.visits().values().all(|count| *count == 1));
    frontier.record("changed-intent", &first, &Default::default());
    assert!(frontier.visits().values().all(|count| *count == 2));
}

#[test]
fn new_frontier_records_unprepared_operation_state_explicitly() {
    let state = SearchFrontier::default();
    let wire = serde_json::to_value(&state).unwrap();
    assert_eq!(wire["operation_state"]["state"], "awaiting_request");
    let restored: SearchFrontier = serde_json::from_value(wire).unwrap();
    assert!(matches!(
        restored.operation_state,
        OperationFrontier::AwaitingRequest
    ));
}

#[test]
fn operation_frontier_reuses_only_the_same_request_and_preserves_old_state_on_revision() {
    use crate::discovery_matcher::operations::{
        MatrixScope, OperationMatrix, OperationSet, RequestIdentity,
    };
    use std::collections::BTreeSet;
    let intent = "Read the sensor and send a notice.";
    let authorized = BTreeSet::from([CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    }]);
    let extraction = RequestIdentity::from_request(intent);
    let scope = MatrixScope::new("session", "generation", &authorized, extraction.clone()).unwrap();
    let mut frontier = OperationFrontier::default();
    assert!(matches!(
        frontier.load(&scope, intent, &authorized),
        Ok(OperationFrontierLoad::AwaitingRequest)
    ));
    let operations = OperationSet::from_request(intent).unwrap();
    let matrix = OperationMatrix::new(
        scope.clone(),
        intent,
        operations,
        &authorized,
        authorized.iter().cloned().collect(),
    )
    .unwrap();
    frontier.commit(&scope, &matrix).unwrap();
    assert!(matches!(
        frontier.load(&scope, intent, &authorized),
        Ok(OperationFrontierLoad::Resume(_))
    ));

    let changed_intent = "Read the sensor only.";
    let changed_scope = MatrixScope::new(
        "session",
        "generation",
        &authorized,
        RequestIdentity::from_request(changed_intent),
    )
    .unwrap();
    assert!(matches!(
        frontier.load(&changed_scope, changed_intent, &authorized),
        Ok(OperationFrontierLoad::ChangedRevision)
    ));
    assert!(
        matches!(
            frontier.load(&scope, intent, &authorized),
            Ok(OperationFrontierLoad::Resume(_))
        ),
        "failed or pending extraction cannot overwrite the previous checkpoint"
    );
    let foreign = MatrixScope::new("other-session", "generation", &authorized, extraction).unwrap();
    assert!(matches!(
        frontier.load(&foreign, intent, &authorized),
        Err(crate::discovery_matcher::operations::OperationError::ScopeMismatch)
    ));
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn replay_of_an_old_completion_cannot_roll_back_operation_revision() {
    use crate::discovery_matcher::operations::{
        MatrixScope, OperationMatrix, OperationSet, RequestIdentity,
    };
    let url = std::env::var("PLASM_TEST_POSTGRES_URL").unwrap();
    let store = DiscoveryStore::connect(&url).await.unwrap();
    store.migrate().await.unwrap();
    let service = DiscoveryService::cached_test_service(store);
    let frontier_key = uuid::Uuid::new_v4().to_string();
    let authorized = std::collections::BTreeSet::new();
    let mut scopes = Vec::new();
    let mut keys = Vec::new();
    for intent in ["Read the sensor.", "Send a notice."] {
        let key = uuid::Uuid::new_v4().to_string();
        let scope = MatrixScope::new(
            "session",
            "generation",
            &authorized,
            RequestIdentity::from_request(intent),
        )
        .unwrap();
        let operations = OperationSet::from_excerpts(intent, &[intent.into()]).unwrap();
        let matrix =
            OperationMatrix::new(scope.clone(), intent, operations, &authorized, vec![]).unwrap();
        let mut next = OperationFrontier::default();
        next.commit(&scope, &matrix).unwrap();
        service
            .store
            .searches()
            .checkpoint_operations(SearchKey(&key), &next)
            .await
            .unwrap();
        service
            .route_frontier(&frontier_key, &key, &[], |_, _, _| async { Ok(receipt()) })
            .await
            .unwrap();
        scopes.push(scope);
        keys.push(key);
    }
    service
        .route_frontier(&frontier_key, &keys[0], &[], |_, _, _| async {
            panic!("completed request must be cached")
        })
        .await
        .unwrap();
    let saved: SearchFrontier = service
        .store
        .searches()
        .frontier(FrontierKey(&frontier_key))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        saved
            .operation_state
            .load(&scopes[1], "Send a notice.", &authorized),
        Ok(OperationFrontierLoad::Resume(_))
    ));
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn same_request_continues_once_per_changed_operation_checkpoint() {
    use crate::discovery_matcher::operations::{
        MatrixScope, OperationMatrix, OperationSet, RequestIdentity,
    };
    let url = std::env::var("PLASM_TEST_POSTGRES_URL").unwrap();
    let store = DiscoveryStore::connect(&url).await.unwrap();
    store.migrate().await.unwrap();
    let service = DiscoveryService::cached_test_service(store);
    let frontier = uuid::Uuid::new_v4().to_string();
    let key = uuid::Uuid::new_v4().to_string();
    let intent = "Read the sensor.";
    let authorized = std::collections::BTreeSet::from([CapabilityRef {
        catalog: "matrix".into(),
        capability: "read".into(),
    }]);
    let scope = MatrixScope::new(
        "session",
        "generation",
        &authorized,
        RequestIdentity::from_request(intent),
    )
    .unwrap();
    let matrix = OperationMatrix::new(
        scope.clone(),
        intent,
        OperationSet::from_excerpts(intent, &[intent.into()]).unwrap(),
        &authorized,
        authorized.iter().cloned().collect(),
    )
    .unwrap();
    let mut state = OperationFrontier::default();
    state.commit(&scope, &matrix).unwrap();
    service
        .store
        .searches()
        .checkpoint_operations(SearchKey(&key), &state)
        .await
        .unwrap();
    service
        .route_frontier(&frontier, &key, &[], |_, _, effective| {
            assert_eq!(effective, key);
            async { Ok(receipt()) }
        })
        .await
        .unwrap();
    service
        .route_frontier(&frontier, &key, &[], |_, _, effective| {
            assert_ne!(effective, key);
            let store = &service.store;
            let state = &state;
            async move {
                store
                    .searches()
                    .checkpoint_operations(SearchKey(&effective), state)
                    .await
                    .unwrap();
                Ok(receipt())
            }
        })
        .await
        .unwrap();
    service
        .route_frontier(&frontier, &key, &[], |_, _, _| async {
            panic!("unchanged checkpoint must reuse its completion")
        })
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn supplemental_changed_intents_share_priority_without_reusing_judgment() {
    let url = std::env::var("PLASM_TEST_POSTGRES_URL").unwrap();
    DiscoveryStore::connect(&url)
        .await
        .unwrap()
        .migrate()
        .await
        .unwrap();
    let (service, _) = services().await;
    let allowed =
        DiscoveryAuthorization::catalogs(std::collections::BTreeSet::from(["matrix".into()]));
    let session = uuid::Uuid::new_v4().to_string();
    let frontier = supplemental_frontier_key(&session, "generation", &allowed, "fixture").unwrap();
    for (suffix, expected) in [("first", 0), ("changed-intent", 1)] {
        let key = format!("{session}-{suffix}");
        service
            .route_frontier(&frontier, &key, &[], |visits, _, _| async move {
                assert_eq!(visits.is_empty(), expected == 0);
                assert!(visits.values().all(|count| *count == expected));
                let mut result = receipt();
                if expected == 0 {
                    for matched in &mut result.matching.matches {
                        matched.choice = matcher::MatchChoice::Unrelated;
                    }
                }
                Ok(result)
            })
            .await
            .unwrap();
    }
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
            .route_frontier(&scope, &key, &[], |visits, _, _| async move {
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
            .route_frontier(&scope, &key, &[], |_, _, _| async {
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
        .route_frontier(&scope, previous.as_ref().unwrap(), &[], |_, _, _| async {
            panic!("reconnecting must not replay completed work")
        })
        .await
        .unwrap();
    let saved: SearchFrontier = recreated
        .store
        .searches()
        .frontier(FrontierKey(&scope))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(saved.completed.len(), 1);
    assert!(saved.visits().values().all(|count| *count == 1));
    let old_scope =
        frontier_key("session", &generation, &allowed, "fixture", first.current()).unwrap();
    let old_key = search_key(&generation, &first, &allowed, "fixture", &[]).unwrap();
    recreated
        .route_frontier(&old_scope, &old_key, &[], |_, _, _| async {
            panic!("returning to a prior request must reuse its completion")
        })
        .await
        .unwrap();
    let old: SearchFrontier = recreated
        .store
        .searches()
        .frontier(FrontierKey(&old_scope))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.completed.len(), 2);
    assert!(old.visits().values().all(|count| *count == 2));
    let returning = changed.derived(first.current().into()).unwrap();
    let returning_key = search_key(&generation, &returning, &allowed, "fixture", &[]).unwrap();
    recreated
        .route_frontier(&old_scope, &returning_key, &[], |visits, _, _| async move {
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
    a.migrate().await.unwrap();
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
    a.route_frontier(&scope, &first, &[], |visits, _, _| async move {
        assert!(visits.is_empty());
        Ok(receipt())
    })
    .await
    .unwrap();
    // Receipt persistence succeeded but the frontier checkpoint did not.
    // The saved completion must repair progress without repeating judgment.
    b.store
        .searches()
        .checkpoint_frontier(FrontierKey(&scope), &SearchFrontier::default())
        .await
        .unwrap();
    b.route_frontier(&scope, &first, &[], |_, _, _| async {
        panic!("duplicate request must not spend work or advance again")
    })
    .await
    .unwrap();
    let mut expected_frontier = SearchFrontier::default();
    expected_frontier.record(&first, &expected, &Default::default());
    let expected_visits = expected_frontier.visits();
    assert!(!expected_visits.is_empty());
    b.route_frontier(&scope, &second, &[], |visits, _, _| async move {
        assert_eq!(visits, expected_visits);
        Ok(receipt())
    })
    .await
    .unwrap();
    let saved: SearchFrontier = a
        .store
        .searches()
        .frontier(FrontierKey(&scope))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(saved.completed.len(), 2);
    assert!(saved.visits().values().all(|count| *count == 2));
}

#[tokio::test]
#[ignore = "requires an isolated PLASM_TEST_POSTGRES_URL with discovery_selector_cache"]
async fn failed_frontier_write_is_repaired_from_the_durable_completion() {
    let (a, b) = services().await;
    let scope = uuid::Uuid::new_v4().to_string();
    let key = uuid::Uuid::new_v4().to_string();
    let name = format!("discovery_reject_{}", uuid::Uuid::new_v4().simple());
    let pool = sqlx::PgPool::connect(&std::env::var("PLASM_TEST_POSTGRES_URL").unwrap())
        .await
        .unwrap();
    let create = format!(
        "CREATE FUNCTION {name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.cache_key = '{scope}' THEN RAISE EXCEPTION 'injected frontier failure'; END IF; RETURN NEW; END $$"
    );
    sqlx::query(&create).execute(&pool).await.unwrap();
    sqlx::query(&format!("CREATE TRIGGER {name} BEFORE INSERT OR UPDATE ON discovery_selector_cache FOR EACH ROW EXECUTE FUNCTION {name}()"))
        .execute(&pool).await.unwrap();
    let calls = AtomicUsize::new(0);
    let failed = a
        .route_frontier(&scope, &key, &[], |_, _, _| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(receipt())
        })
        .await;
    assert!(matches!(failed, Err(DiscoveryServiceError::Store(_))));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        a.store
            .searches()
            .completion(SearchKey(&key))
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        a.store
            .searches()
            .frontier(FrontierKey(&scope))
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query(&format!("DROP TRIGGER {name} ON discovery_selector_cache"))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(&format!("DROP FUNCTION {name}()"))
        .execute(&pool)
        .await
        .unwrap();
    b.route_frontier(&scope, &key, &[], |_, _, _| async {
        panic!("repair must reuse the durable completion")
    })
    .await
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let restored = b
        .store
        .searches()
        .frontier(FrontierKey(&scope))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.completed.len(), 1);
    pool.close().await;
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
            a.route_frontier(&scope, &format!("{scope}:first"), &[], |_, _, _| async {
                started.notify_one();
                release.notified().await;
                Ok(receipt())
            })
            .await
        })
    };
    started.notified().await;
    let next = format!("{scope}:next");
    let mut follower = Box::pin(
        b.route_frontier(&scope, &next, &[], |visits, _, _| async move {
            assert!(!visits.is_empty());
            assert!(visits.values().all(|count| *count == 1));
            Ok(receipt())
        }),
    );
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
                        .searches()
                        .reserve(SearchKey(&key))
                        .await?
                        .is_fresh()
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
            .searches()
            .reserve(SearchKey(&key))
            .await?
            .is_fresh()
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
                a.route_frontier(&format!("{key}:frontier"), &key, &[], |visits, _, _| {
                    assert!(visits.is_empty());
                    async {
                        assert!(
                            a.store
                                .searches()
                                .reserve(SearchKey(&key))
                                .await?
                                .is_fresh()
                        );
                        a.store
                            .searches()
                            .checkpoint(SearchKey(&key), &progress(1))
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
        let mut follower = Box::pin(b.route_frontier(&scope, &key, &[], |visits, _, _| {
            assert!(visits.is_empty());
            async {
                let SearchAdmission::PreviouslyStarted(Some(saved)) =
                    b.store.searches().reserve(SearchKey(&key)).await?
                else {
                    panic!("spent search must retain its progress");
                };
                assert_eq!(saved.accounting().requests, 1);
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
                        .searches()
                        .reserve(SearchKey(&key))
                        .await?
                        .is_fresh()
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
                        .searches()
                        .reserve(SearchKey(&key))
                        .await?
                        .is_fresh()
                );
                tokio::task::yield_now().await;
                a.store
                    .searches()
                    .checkpoint(SearchKey(&key), &progress(1))
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
