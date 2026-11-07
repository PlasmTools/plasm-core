use super::{
    tests::{answer, fixture},
    *,
};
use plasm_runtime::provider_transport::ProviderTransportError;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn successful_response_persistence_does_not_expire_provider_deadline() {
    exercise_response_checkpoint(false, false, 31).await;
}

#[tokio::test]
async fn recovered_response_persistence_does_not_replay_provider() {
    exercise_response_checkpoint(true, false, 31).await;
}

#[tokio::test]
async fn failed_response_persistence_is_checkpoint_failure_without_replay() {
    exercise_response_checkpoint(false, true, 0).await;
}

async fn exercise_response_checkpoint(fail_first: bool, fail_write: bool, delay_secs: u64) {
    use crate::discovery_service::decision_checkpoint::{DecisionCache, DecisionCheckpoint};
    use futures_util::FutureExt;
    struct SlowCache {
        saved: Mutex<Option<String>>,
        delay_secs: u64,
        fail_write: bool,
    }
    #[async_trait::async_trait]
    impl DecisionCache for SlowCache {
        type Owner = ();
        async fn claim(&self, _: &str) -> Result<Self::Owner, DecisionProviderError> {
            Ok(())
        }
        async fn load(&self, _: &str) -> Result<Option<String>, DecisionProviderError> {
            Ok(self.saved.lock().unwrap().clone())
        }
        async fn save(
            &self,
            _: &str,
            raw: &str,
            _: &mut Self::Owner,
        ) -> Result<(), DecisionProviderError> {
            tokio::time::sleep(std::time::Duration::from_secs(self.delay_secs)).await;
            if self.fail_write {
                return Err(Box::new(std::io::Error::other(
                    "abstract checkpoint failed",
                )));
            }
            *self.saved.lock().unwrap() = Some(raw.into());
            Ok(())
        }
    }
    let cache = SlowCache {
        saved: Mutex::new(None),
        delay_secs,
        fail_write,
    };
    let locks = crate::discovery_scheduling::DecisionLocks::default();
    let checkpoint = DecisionCheckpoint::new(&cache, &locks);
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let intent = IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap();
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let mut calls = 0;
    let cache_key = Mutex::new(None::<String>);
    let result = select_indexed(
        SearchContext {
            visits: &Default::default(),
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "response-persistence",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &[],
        },
        &index,
        |request| {
            calls += 1;
            let fail = fail_first && calls == 1;
            *cache_key.lock().unwrap() = Some(request.cache_key().into());
            let checkpoint = &checkpoint;
            async move {
                let raw = answer(&request, "relevant");
                checkpoint
                    .receive(request, || async {
                        if fail {
                            Err(rejected(503, None))
                        } else {
                            Ok(raw)
                        }
                    })
                    .await
            }
        },
        |progress| {
            let key = cache_key.lock().unwrap().clone();
            let checkpoint = &checkpoint;
            let locks = &locks;
            async move {
                if matches!(progress.state, ProgressState::Settled) {
                    assert!(
                        locks.claim(key.as_ref().unwrap()).now_or_never().is_none(),
                        "identical cache fills must wait for persistence"
                    );
                }
                checkpoint.persist().await
            }
        },
    )
    .await;
    if fail_write {
        assert!(
            matches!(result, Err(DiscoveryMatcherError::Checkpoint(_))),
            "{result:?}"
        );
        assert!(cache.saved.lock().unwrap().is_none());
    } else {
        assert!(
            result.is_ok(),
            "a received valid response must survive its durable checkpoint: {result:?}"
        );
        assert!(cache.saved.lock().unwrap().is_some());
        let (_, _, receipt) = result.unwrap();
        assert_eq!(receipt.requests, 1 + usize::from(fail_first));
    }
    assert_eq!(
        calls,
        1 + usize::from(fail_first),
        "persistence cannot replay a provider request"
    );
    assert!(
        locks
            .claim(cache_key.lock().unwrap().as_ref().unwrap())
            .now_or_never()
            .is_some(),
        "completion or failure must release cache-fill ownership"
    );
}

#[tokio::test]
async fn stalled_transport_recovers_through_online_admission_without_replaying_effects() {
    use plasm_runtime::provider_transport::{
        request_attempt, ProviderRetryPolicy, ReqwestProviderTransport,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    let effects = Arc::new(AtomicUsize::new(0));
    let effect_calls = effects.clone();
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/judge", listener.local_addr().unwrap());
    let router = axum::Router::new()
        .route("/effect", axum::routing::post(move || { effect_calls.fetch_add(1, Ordering::SeqCst); async { "effect" } }))
        .route("/judge", axum::routing::post(move |body: String| {
            let seen = seen.clone();
            async move {
                let first = { let mut requests = seen.lock().unwrap(); requests.push(body.clone()); requests.len() == 1 };
                if first { std::future::pending::<()>().await; }
                let body: serde_json::Value = serde_json::from_str(&body).unwrap();
                let answers: BTreeMap<_,_> = body["questions"].as_object().unwrap().keys().map(|key| (key, serde_json::json!({"type":"choice","choice":"relevant","confidence":1.0,"probabilities":{"relevant":1.0,"uncertain":0.0,"unrelated":0.0}}))).collect();
                serde_json::json!({"model":RELEVANCE_MODEL,"provider":"OpenAI","answers":answers}).to_string()
            }
        }));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let transport = ReqwestProviderTransport::new(reqwest::Client::new(), endpoint, "abstract");
    let began = std::time::Instant::now();
    let mut attempt = 0;
    let result = select(
        SearchContext {
            visits: &Default::default(),
            model: RELEVANCE_MODEL,
            intent: &IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap(),
            generation: "transport-recovery",
            catalogs: &fixture(),
            deployments: &Default::default(),
            allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            exposed: &[],
        },
        |issued| {
            let transport = &transport;
            attempt += 1;
            let attempt = attempt;
            async move {
                request_attempt(
                    transport,
                    issued.body(),
                    attempt,
                    ProviderRetryPolicy::discovery_attempt().attempt_timeout,
                    |_| Ok(()),
                )
                .await
                .map_err(|e| Box::new(e) as DecisionProviderError)
            }
        },
    )
    .await;
    server.abort();
    let (_, matching, receipt) = result.unwrap();
    let bodies = requests.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0], bodies[1]);
    assert_eq!(receipt.requests, 2);
    assert_eq!(receipt.input_bytes, 2 * bodies[0].len());
    assert!(receipt.input_bytes <= 48_000);
    assert!(!matching.matches.is_empty());
    assert!(began.elapsed() < std::time::Duration::from_secs(30));
    assert_eq!(effects.load(Ordering::SeqCst), 0);
}

fn rejected(status: u16, retry_after: Option<std::time::Duration>) -> DecisionProviderError {
    Box::new(ProviderTransportError::ProviderRejected {
        status: reqwest::StatusCode::from_u16(status).unwrap(),
        message: "Abstract provider response".into(),
        retry_after,
    })
}

#[tokio::test]
async fn slow_retry_checkpoint_cannot_dispatch_after_deadline() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let intent = IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap();
    let context = SearchContext {
        visits: &Default::default(),
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "slow-checkpoint",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
    };
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let mut calls = 0;
    let error = select_indexed(
        context,
        &index,
        |_| {
            calls += 1;
            async { Err(rejected(503, None)) }
        },
        |p| async move {
            if p.accounting.requests == 2 && matches!(p.state, ProgressState::Admitted) {
                tokio::time::sleep(std::time::Duration::from_secs(31)).await;
            }
            Ok(())
        },
    )
    .await
    .unwrap_err();
    let DiscoveryMatcherError::Provider(error) = error else {
        panic!("expected deadline error")
    };
    assert!(matches!(
        error.downcast_ref::<ProviderTransportError>(),
        Some(ProviderTransportError::DeadlineExpired)
    ));
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn pending_provider_is_cancelled_at_the_discovery_deadline() {
    let error = select(
        SearchContext {
            visits: &Default::default(),
            model: RELEVANCE_MODEL,
            intent: &IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap(),
            generation: "deadline",
            catalogs: &fixture(),
            deployments: &Default::default(),
            allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            exposed: &[],
        },
        |_| std::future::pending(),
    )
    .await
    .unwrap_err();
    let DiscoveryMatcherError::Provider(error) = error else {
        panic!("expected provider timeout")
    };
    assert!(matches!(
        error.downcast_ref::<ProviderTransportError>(),
        Some(ProviderTransportError::DeadlineExpired)
    ));
}

#[tokio::test]
async fn terminal_failures_and_retry_after_do_not_escape_attempt_or_time_limits() {
    for (status, retry_after, expected) in [
        (503, None, 3),
        (429, Some(std::time::Duration::from_secs(31)), 1),
        (503, Some(std::time::Duration::from_secs(u64::MAX)), 1),
        (401, None, 1),
        (403, None, 1),
        (400, None, 1),
    ] {
        let catalogs = fixture();
        let deployments = Default::default();
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let intent = IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap();
        let context = SearchContext {
            visits: &Default::default(),
            model: RELEVANCE_MODEL,
            intent: &intent,
            generation: "retry-failure",
            catalogs: &catalogs,
            deployments: &deployments,
            allowed: &allowed,
            exposed: &[],
        };
        let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
        let mut calls = 0;
        let mut last = None;
        let error = select_indexed(
            context,
            &index,
            |_| {
                calls += 1;
                async move { Err(rejected(status, retry_after)) }
            },
            |p| {
                last = Some(p);
                async { Ok(()) }
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(error, DiscoveryMatcherError::Provider(_)));
        assert_eq!(calls, expected);
        let last = last.unwrap();
        assert!(matches!(last.state, ProgressState::Failed { .. }));
        assert_eq!(last.accounting.requests, expected);
        assert_eq!(last.accounting.missing_usage, expected);
        assert!(last.accounting.input_bytes <= 48_000);
    }
}

#[tokio::test]
async fn failed_retry_checkpoint_prevents_the_next_dispatch() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let intent = IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap();
    let context = SearchContext {
        visits: &Default::default(),
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "retry-checkpoint",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
    };
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let mut calls = 0;
    let error = select_indexed(
        context,
        &index,
        |_| {
            calls += 1;
            async { Err(rejected(503, None)) }
        },
        |p| async move {
            if p.accounting.requests == 2 {
                Err(
                    Box::new(std::io::Error::other("abstract checkpoint unavailable"))
                        as DecisionProviderError,
                )
            } else {
                Ok(())
            }
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(error, DiscoveryMatcherError::Checkpoint(_)));
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn malformed_success_is_not_a_transport_retry() {
    let mut calls = 0;
    let error = select(
        SearchContext {
            visits: &Default::default(),
            model: RELEVANCE_MODEL,
            intent: &IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap(),
            generation: "malformed",
            catalogs: &fixture(),
            deployments: &Default::default(),
            allowed: &DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()])),
            exposed: &[],
        },
        |_| {
            calls += 1;
            async { Ok("not a decision response".into()) }
        },
    )
    .await
    .unwrap_err();
    assert!(!matches!(error, DiscoveryMatcherError::Provider(_)));
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn cancellation_during_retry_keeps_the_second_reservation() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let intent = IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap();
    let context = SearchContext {
        visits: &Default::default(),
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "retry-cancel",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
    };
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let snapshots = Arc::new(Mutex::new(Vec::<SearchProgress>::new()));
    let saved = snapshots.clone();
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let mut calls = 0;
    let future = select_indexed(
        context,
        &index,
        |_| {
            calls += 1;
            let call = calls;
            let tx = tx.clone();
            async move {
                if call == 1 {
                    Err(rejected(503, None))
                } else {
                    tx.send(()).await.unwrap();
                    std::future::pending().await
                }
            }
        },
        move |p| {
            saved.lock().unwrap().push(p);
            async { Ok(()) }
        },
    );
    tokio::select! {
        result = future => panic!("must cancel in-flight retry: {result:?}"),
        _ = rx.recv() => {},
    }
    let snapshots = snapshots.lock().unwrap();
    let last = snapshots.last().unwrap();
    assert_eq!(last.accounting.requests, 2);
    assert_eq!(last.accounting.missing_usage, 1);
    assert_eq!(
        last.accounting.input_bytes,
        snapshots[0].accounting.input_bytes * 2
    );
    assert!(last.judgments.is_empty());
}

#[tokio::test]
async fn transient_retry_is_identical_and_checkpointed_before_each_dispatch() {
    let catalogs = fixture();
    let deployments = Default::default();
    let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
    let intent = IntentProvenance::from_turns(["Produce spectrum".into()]).unwrap();
    let context = SearchContext {
        visits: &Default::default(),
        model: RELEVANCE_MODEL,
        intent: &intent,
        generation: "retry-matrix",
        catalogs: &catalogs,
        deployments: &deployments,
        allowed: &allowed,
        exposed: &[],
    };
    let index = CatalogueSearch::new(&catalogs, &deployments, &allowed).unwrap();
    let snapshots = Arc::new(Mutex::new(Vec::<SearchProgress>::new()));
    let saved = snapshots.clone();
    let before_dispatch = snapshots.clone();
    let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = bodies.clone();
    let (_, _, receipt) = select_indexed(
        context,
        &index,
        move |request| {
            let mut bodies = seen.lock().unwrap();
            bodies.push(request.body().to_owned());
            let count = bodies.len();
            let snapshots = before_dispatch.lock().unwrap();
            let last = snapshots.last().unwrap();
            assert_eq!(last.accounting.requests, count);
            assert_eq!(last.accounting.input_bytes, request.body().len() * count);
            assert!(matches!(last.state, ProgressState::Admitted));
            async move {
                if count < 3 {
                    Err(Box::new(ProviderTransportError::ProviderRejected {
                        status: reqwest::StatusCode::SERVICE_UNAVAILABLE,
                        message: "Abstract temporary outage".into(),
                        retry_after: None,
                    }) as DecisionProviderError)
                } else {
                    Ok(answer(&request, "uncertain"))
                }
            }
        },
        move |progress| {
            saved.lock().unwrap().push(progress);
            async { Ok(()) }
        },
    )
    .await
    .unwrap();
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    assert!(bodies.iter().all(|body| body == &bodies[0]));
    assert_eq!(receipt.requests, 3);
    assert_eq!(receipt.input_bytes, bodies[0].len() * 3);
    assert!(receipt.input_bytes <= 48_000);
    assert!(receipt.missing_usage >= 2);
}
