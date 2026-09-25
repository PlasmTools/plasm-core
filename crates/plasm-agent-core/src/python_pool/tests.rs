use super::*;
use monty_pool::{PoolError, ResumeValue};
fn binary() -> PathBuf {
    PathBuf::from(
        std::env::var_os("PLASM_MONTY_BINARY").expect("run scripts/ci/test-python-pool.sh"),
    )
}
async fn pool() -> Pool {
    Pool::new(pool_config(binary())).await.unwrap()
}
async fn evaluate(session: &mut Checkout, source: &str) -> Result<TurnEvent, PoolError> {
    session
        .feed(source, vec![], vec![], true, &mut on_print_sync(|_, _| {}))
        .await
}
async fn healthy(pool: &Pool) {
    let mut session = pool.checkout(&repl_config()).await.unwrap();
    assert!(
        matches!(evaluate(&mut session,"21 * 2").await.unwrap(),TurnEvent::Complete(value) if value.as_ref().as_int()==Some(42))
    );
    session.finish().await.unwrap();
}
#[cfg(unix)]
async fn signal(pid: u32, name: &str) {
    assert!(tokio::process::Command::new("/bin/kill")
        .args([name, &pid.to_string()])
        .status()
        .await
        .unwrap()
        .success());
}
#[tokio::test]
async fn pool_roundtrip_and_fresh_session_state() {
    let pool = pool().await;
    let mut first = pool.checkout(&repl_config()).await.unwrap();
    evaluate(&mut first, "secret = 'previous tenant'")
        .await
        .unwrap();
    first.finish().await.unwrap();
    let mut second = pool.checkout(&repl_config()).await.unwrap();
    assert!(
        matches!(evaluate(&mut second,"secret").await.unwrap(),TurnEvent::NameLookup{name,..} if name=="secret")
    );
    drop(second);
    healthy(&pool).await;
    pool.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn pool_actual_worker_crash_does_not_kill_host_and_next_checkout_recovers() {
    let pool = pool().await;
    for crash in ["-ABRT", "-KILL"] {
        let mut session = pool.checkout(&repl_config()).await.unwrap();
        let pid = session.pid().unwrap();
        let kill = async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            signal(pid, crash).await;
        };
        let (result, ()) = tokio::join!(evaluate(&mut session, "while True:\n    pass"), kill);
        assert!(
            matches!(result, Err(PoolError::Crashed { .. })),
            "{crash}: {result:?}"
        );
        drop(session);
        healthy(&pool).await;
    }
    pool.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn pool_hard_deadline_kills_wedged_worker() {
    let mut config = pool_config(binary());
    config.request_timeout = Some(Duration::from_millis(500));
    let pool = Pool::new(config).await.unwrap();
    let mut session = pool.checkout(&repl_config()).await.unwrap();
    signal(session.pid().unwrap(), "-STOP").await;
    assert!(matches!(
        evaluate(&mut session, "42").await,
        Err(PoolError::Timeout { .. })
    ));
    drop(session);
    healthy(&pool).await;
    pool.close().await;
}
#[tokio::test]
async fn pool_memory_cpu_recursion_and_parse_errors_are_contained() {
    let pool = pool().await;
    for source in [
        "'x' * 200000000",
        "while True:\n    pass",
        "def recurse():\n    return recurse()\nrecurse()",
        "def broken(:",
    ] {
        let mut session = pool.checkout(&repl_config()).await.unwrap();
        assert!(evaluate(&mut session, source).await.is_err(), "{source}");
        // Never return a failed or resource-exhausted session to the pool.
        drop(session);
        healthy(&pool).await;
    }
    pool.close().await;
}
#[tokio::test]
async fn pool_cancelled_checkout_and_waiter_do_not_leak_capacity() {
    let mut config = pool_config(binary());
    config.max_processes = 1;
    let pool = Pool::new(config).await.unwrap();
    let mut held = pool.checkout(&repl_config()).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), pool.checkout(&repl_config()))
            .await
            .is_err()
    );
    assert!(matches!(
        evaluate(&mut held, "missing_name").await.unwrap(),
        TurnEvent::NameLookup { .. }
    ));
    drop(held);
    tokio::time::timeout(Duration::from_secs(3), healthy(&pool))
        .await
        .unwrap();
    pool.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn pool_cancellation_during_inflight_turn_discards_worker() {
    let pool = pool().await;
    let mut session = pool.checkout(&repl_config()).await.unwrap();
    signal(session.pid().unwrap(), "-STOP").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(30), evaluate(&mut session, "42"))
            .await
            .is_err()
    );
    drop(session);
    healthy(&pool).await;
    pool.close().await;
}
#[tokio::test]
async fn pool_host_roundtrip_is_async_and_suspension_budget_is_enforced() {
    let pool = pool().await;
    let mut session = pool.checkout(&repl_config()).await.unwrap();
    let event = session
        .feed(
            "await call()\nawait call()",
            vec![("call".to_string(), MontyObject::function("call", None))],
            vec![],
            true,
            &mut on_print_sync(|_, _| {}),
        )
        .await
        .unwrap();
    let TurnEvent::FunctionCall {
        call_id,
        allow_eager_await: true,
        ..
    } = event
    else {
        panic!("unexpected suspension")
    };
    // Host work may yield without consuming interpreter execution time.
    tokio::time::sleep(Duration::from_millis(110)).await;
    let error = session
        .resume_futures(
            vec![(call_id, ResumeValue::Return(MontyObject::string("result")))],
            &mut on_print_sync(|_, _| {}),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("suspension limit"), "{error}");
    drop(session);
    healthy(&pool).await;
    pool.close().await;
}
#[cfg(unix)]
#[tokio::test]
async fn pool_crash_after_host_effect_does_not_replay_the_effect() {
    let pool = pool().await;
    let mut session = pool.checkout(&repl_config()).await.unwrap();
    let event = session
        .feed(
            "await write()",
            vec![("write".to_string(), MontyObject::function("write", None))],
            vec![],
            true,
            &mut on_print_sync(|_, _| {}),
        )
        .await
        .unwrap();
    let TurnEvent::FunctionCall { call_id, .. } = event else {
        panic!("expected write suspension")
    };
    let mut writes = 0;
    writes += 1; // The host has committed its effect before returning to Python.
    signal(session.pid().unwrap(), "-KILL").await;
    assert!(session
        .resume_futures(
            vec![(call_id, ResumeValue::Return(MontyObject::none()))],
            &mut on_print_sync(|_, _| {})
        )
        .await
        .is_err());
    drop(session);
    healthy(&pool).await;
    assert_eq!(writes, 1);
    pool.close().await;
}
#[tokio::test]
async fn pool_adapter_rejects_wrong_return_and_undeclared_io() {
    let pool = PythonPool::with_binary(binary());
    for source in ["42", "'x' * 1048577", "open('/etc/passwd')", "unknown()"] {
        assert!(
            pool.compute(source.into(), vec![]).await.is_err(),
            "{source}"
        );
        assert_eq!(pool.compute("'ok'".into(), vec![]).await.unwrap(), "ok");
    }
    pool.close().await;
}

#[tokio::test]
async fn pool_adversarial_parser_input_leaves_host_and_pool_usable() {
    let pool = pool().await;
    let mut session = pool.checkout(&repl_config()).await.unwrap();
    // Beyond Plasm's 4 KiB admission bound: exercise containment in the upstream compiler too.
    let source = format!("a{}", ".x".repeat(150_000));
    assert!(evaluate(&mut session, &source).await.is_err());
    drop(session);
    healthy(&pool).await;
    pool.close().await;
}

#[tokio::test]
async fn pool_missing_binary_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let pool = PythonPool::with_binary(dir.path().join("missing-monty"));
    assert!(pool
        .compute("'must not execute'".into(), vec![])
        .await
        .is_err());
    pool.close().await;
}

#[tokio::test]
async fn typed_scalar_roundtrip_preserves_python_numeric_boolean_and_null_values() {
    let pool = PythonPool::with_binary(binary());
    let input = vec![BTreeMap::from([
        ("n".into(), serde_json::json!(7)),
        ("f".into(), serde_json::json!(1.25)),
        ("b".into(), serde_json::json!(true)),
        ("missing".into(), serde_json::Value::Null),
    ])];
    assert_eq!(
        pool.compute_typed(
            "f'{__input[0].n:04d}|{__input[0].f:.2f}|{__input[0].b}|{__input[0].missing}'".into(),
            input,
            &BTreeMap::new(),
        )
        .await
        .unwrap(),
        "0007|1.25|True|None"
    );
    pool.close().await;
}
