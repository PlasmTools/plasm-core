use super::{
    tests::{answer, fixture},
    *,
};
use crate::decision_transport::DecisionTransportError;
use std::sync::{Arc, Mutex};

fn rejected(status: u16, retry_after: Option<std::time::Duration>) -> DecisionProviderError {
    Box::new(DecisionTransportError::ProviderRejected {
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
        error.downcast_ref::<DecisionTransportError>(),
        Some(DecisionTransportError::DeadlineExpired)
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
        error.downcast_ref::<DecisionTransportError>(),
        Some(DecisionTransportError::DeadlineExpired)
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
                    Err(Box::new(DecisionTransportError::ProviderRejected {
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
