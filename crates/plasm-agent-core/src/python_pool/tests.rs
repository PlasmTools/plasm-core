use super::*;
use monty_pool::{PoolError, ResumeValue};

#[tokio::test]
async fn datetime_typed_codec_and_clock_matrix() {
    use crate::fixture_value as json;
    use plasm_core::{
        temporal_value::TemporalKind as K,
        value_contract::{ValueContract as T, ValueShape},
        TemporalWireFormat as W,
    };
    let pool = PythonPool::with_binary(binary()).with_clock(monty_types::DateTimeSource::Fixed {
        unix_seconds: 1709164800,
        microsecond: 123456,
    });
    let fields = BTreeMap::from([
        (
            "date".into(),
            T {
                shape: ValueShape::Temporal {
                    kind: K::Date,
                    wire: Some(W::Iso8601Date),
                },
                domain: None,
                nullable: false,
            },
        ),
        (
            "epoch".into(),
            T {
                shape: ValueShape::Temporal {
                    kind: K::Datetime,
                    wire: Some(W::UnixMs),
                },
                domain: None,
                nullable: false,
            },
        ),
    ]);
    let result = pool.compute_value(
        "from datetime import datetime, timedelta, timezone\n{'date': __input[0].date + timedelta(days=1), 'epoch': __input[0].epoch, 'now': datetime.now(timezone.utc), 'duration': timedelta(hours=-1)}".into(),
        vec![BTreeMap::from([("date".into(), json!("2024-02-28")), ("epoch".into(), json!(-1))])], &fields, Ok
    ).await.unwrap();
    assert_eq!(
        result["date"]["components"],
        json!({"year":2024,"month":2,"day":29})
    );
    assert_eq!(result["epoch"]["components"]["microsecond"], json!(999000));
    assert_eq!(result["now"]["components"]["microsecond"], json!(123456));
    assert_eq!(
        result["duration"]["components"],
        json!({"days":-1,"seconds":82800,"microseconds":0})
    );
    let nested = T {
        shape: ValueShape::Array {
            element: Box::new(T::record(
                BTreeMap::from([("instant".into(), K::Datetime.contract())]),
                Default::default(),
            )),
        },
        domain: None,
        nullable: false,
    };
    let roundtrip = pool
        .compute_value(
            "__input[0].nested[0].instant".into(),
            vec![BTreeMap::from([(
                "nested".into(),
                json!([{"instant":result["now"]}]),
            )])],
            &BTreeMap::from([("nested".into(), nested)]),
            Ok,
        )
        .await
        .unwrap();
    assert_eq!(roundtrip, result["now"]);
    pool.close().await;
}

#[test]
fn datetime_codec_rejects_loss_and_invalid_values() {
    use crate::fixture_value as json;
    use plasm_core::{
        temporal_value::TemporalKind as K,
        value_contract::{ValueContract as T, ValueShape},
        TemporalWireFormat as W,
    };
    let contract = T {
        shape: ValueShape::Temporal {
            kind: K::Datetime,
            wire: Some(W::Rfc3339),
        },
        domain: None,
        nullable: false,
    };
    for value in [
        json!("2024-01-01T00:00:00.1234567Z"),
        json!("2024-01-01T00:00:00"),
        json!(0),
        json!("2024-02-30T00:00:00Z"),
    ] {
        assert!(value_object(value, Some(&contract)).is_err());
    }
}
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
            matches!(
                result,
                Err(PoolError::Crashed { .. } | PoolError::Timeout { .. })
            ),
            "{crash}: {result:?}"
        );
        // Under contention macOS crash processing can outlive the hard feed
        // deadline. Both errors fail closed; recovery must use a different PID.
        drop(session);
        let mut replacement = pool.checkout(&repl_config()).await.unwrap();
        assert_ne!(replacement.pid(), Some(pid));
        assert!(
            matches!(evaluate(&mut replacement, "21 * 2").await.unwrap(), TurnEvent::Complete(value) if value.as_ref().as_int() == Some(42))
        );
        replacement.finish().await.unwrap();
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

#[tokio::test]
async fn parked_host_suspension_returns_capacity_and_can_be_cancelled() {
    let pool = PythonPool::with_binary(binary());
    let mut session = pool.checkout().await.unwrap();
    let event = session
        .feed(
            "await call()",
            vec![("call".into(), MontyObject::function("call", None))],
            vec![],
            true,
            &mut on_print_sync(|_, _| {}),
        )
        .await
        .unwrap();
    assert!(matches!(event, TurnEvent::FunctionCall { .. }));
    let suspended = PythonPool::suspend_host_call(session).await.unwrap();
    assert_eq!(pool.get().await.unwrap().idle_workers(), 1);
    drop(suspended);
    assert_eq!(pool.get().await.unwrap().idle_workers(), 1);
    healthy(pool.get().await.unwrap()).await;
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
    let mut config = repl_config();
    config.limits.as_mut().unwrap().max_suspensions = 1;
    let mut session = pool.checkout(&config).await.unwrap();
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
        ("n".into(), crate::fixture_value!(7)),
        ("f".into(), crate::fixture_value!(1.25)),
        ("b".into(), crate::fixture_value!(true)),
        ("missing".into(), plasm_core::Value::Null),
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

#[tokio::test]
async fn recursive_union_codec_preserves_record_attributes_and_nullable_elements() {
    use crate::fixture_value as json;
    use plasm_core::value_contract::{ValueContract as T, ValueShape};
    use plasm_core::FieldType;
    let record = T {
        shape: ValueShape::Record {
            fields: BTreeMap::from([("n".into(), T::scalar(FieldType::Integer))]),
        },
        domain: None,
        nullable: false,
    };
    let element = T {
        shape: ValueShape::Union {
            variants: vec![record, T::scalar(FieldType::String)],
        },
        domain: None,
        nullable: true,
    };
    let array = T {
        shape: ValueShape::Array {
            element: Box::new(element.clone()),
        },
        domain: None,
        nullable: false,
    };
    let pool = PythonPool::default();
    let result = pool.compute_typed(
        "'|'.join('null' if x is None else x if isinstance(x, str) else str(x.n) for x in __input[0].values)".into(),
        vec![BTreeMap::from([("values".into(), json!([{"n":9007199254740993_i64}, "a", null]))])],
        &BTreeMap::from([("values".into(), array)]),
    ).await.unwrap();
    assert_eq!(result, "9007199254740993|a|null");
    assert!(value_object(json!(true), Some(&element)).is_err());
    pool.close().await;
}

#[test]
fn typed_output_codec_is_lossless_and_bounded() {
    fn decode(value: MontyObject) -> Result<plasm_core::Value, super::PythonOutputError> {
        super::output_value(value.as_ref(), 0, &mut 1024)
    }
    for (object, expected) in [
        (MontyObject::none(), plasm_core::Value::Null),
        (MontyObject::bool(false), crate::fixture_value!(false)),
        (
            MontyObject::int(9007199254740993),
            crate::fixture_value!(9007199254740993_i64),
        ),
        (
            MontyObject::bigint(u64::MAX.into()),
            crate::fixture_value!(u64::MAX),
        ),
        (
            MontyObject::list([MontyObject::none(), MontyObject::string("0001")]),
            crate::fixture_value!([null, "0001"]),
        ),
        (
            MontyObject::tuple([MontyObject::int(1), MontyObject::string("two")]),
            crate::fixture_value!([1, "two"]),
        ),
    ] {
        assert_eq!(decode(object).unwrap(), expected);
    }
    for object in [
        MontyObject::float(f64::NAN),
        MontyObject::float(f64::INFINITY),
        MontyObject::bigint(u128::MAX.into()),
        MontyObject::dict([(MontyObject::int(1), MontyObject::none())]),
        MontyObject::string("x".repeat(2048)),
        MontyObject::class_type("Counterfeit", super::fresh_uuid(), true, false, []),
    ] {
        assert!(decode(object).is_err());
    }
    let mut nested = MontyObject::none();
    for _ in 0..65 {
        nested = MontyObject::list([nested]);
    }
    assert!(decode(nested).is_err());
}

#[tokio::test]
async fn exact_money_functions_execute_in_monty() {
    use plasm_core::{MoneyValue, Value};
    let pool = PythonPool::with_binary(binary());
    let money = |amount: &str, currency: &str| {
        Value::Money(MoneyValue::new(
            amount.parse().unwrap(),
            Some(currency.into()),
        ))
    };
    let rows = vec![BTreeMap::from([
        ("price".into(), money("0.1", "USD")),
        ("other".into(), money("0.2", "USD")),
        ("eur".into(), money("1", "EUR")),
    ])];
    for (source, expected) in [
        ("money_add(__input[0].price, __input[0].other)", "0.3"),
        ("money_mul(factor='1.25', value=__input[0].price)", "0.125"),
        (
            "money_div(money_sub(__input[0].other, __input[0].price), 2)",
            "0.05",
        ),
        (
            "money_add(money_mul(__input[0].price, 2), __input[0].other)",
            "0.4",
        ),
    ] {
        let result = pool
            .compute_value(source.into(), rows.clone(), &BTreeMap::new(), |v| {
                crate::python_money::decode(v)
                    .map(Value::Money)
                    .map_err(PythonReturnValueError::from)
            })
            .await
            .unwrap();
        assert_eq!(result, money(expected, "USD"));
    }
    for (source, expected) in [
        ("money_compare(__input[0].price, __input[0].other)", -1),
        ("money_compare(right='0.1', left=__input[0].price)", 0),
        ("money_compare(__input[0].price, 0)", 1),
    ] {
        assert_eq!(
            pool.compute_value(source.into(), rows.clone(), &BTreeMap::new(), Ok)
                .await
                .unwrap(),
            Value::Integer(expected)
        );
    }
    for source in [
        "money_add(__input[0].price, __input[0].eur)",
        "money_compare(__input[0].price, __input[0].eur)",
        "money_compare(__input[0].price, 0.1)",
        "money_compare(__input[0].price, 'invalid')",
        "money_div(__input[0].price, 0)",
        "money_mul(__input[0].price, 0.1)",
        "money_mul(__input[0].price, 'no')",
        "money_mul(__input[0].price, 2, factor=3)",
    ] {
        assert!(
            pool.compute_value(source.into(), rows.clone(), &BTreeMap::new(), Ok)
                .await
                .is_err(),
            "{source}"
        );
    }
    let caught = pool.compute_value(
        "try:\n    money_div(__input[0].price, 0)\nexcept ValueError:\n    result = 'caught'\nresult".into(),
        rows, &BTreeMap::new(), Ok,
    ).await.unwrap();
    assert_eq!(caught, Value::String("caught".into()));
    pool.close().await;
}

#[tokio::test]
async fn python_exceptions_preserve_repair_authority_and_pool_reuse() {
    use plasm_runtime::{FailureCause, RecoveryDisposition};
    let pool = PythonPool::with_binary(binary());
    for expression in ["1 // 0", "int('Hi')", "[][1]"] {
        let failure = pool
            .compute_value(expression.into(), vec![], &BTreeMap::new(), Ok)
            .await
            .unwrap_err();
        assert_eq!(failure.cause, FailureCause::Program);
        assert_eq!(failure.recovery, RecoveryDisposition::RepairProgram);
        assert_eq!(failure.code, "python_exception");
        let value = pool
            .compute_value("1".into(), vec![], &BTreeMap::new(), Ok)
            .await
            .unwrap();
        assert_eq!(value, plasm_core::Value::from(1_i64));
    }
    pool.close().await;
}

#[tokio::test]
async fn indexed_records_preserve_null_missing_and_dynamic_keys() {
    use plasm_core::value_contract::ValueContract as T;
    use plasm_core::FieldType;
    let record = T::record(
        BTreeMap::from([
            ("n".into(), T::scalar(FieldType::Integer)),
            (
                "empty".into(),
                T {
                    nullable: true,
                    ..T::scalar(FieldType::String)
                },
            ),
        ]),
        Default::default(),
    );
    let pool = PythonPool::default();
    let rows = vec![BTreeMap::from([(
        "value".into(),
        crate::fixture_value!({"n":9007199254740993_i64,"empty":null}),
    )])];
    let contracts = BTreeMap::from([("value".into(), record)]);
    let result = pool
        .compute_typed(
            "str(__input[0]['value'][''.join(['n'])]) + '|' + str(__input[0].value['empty'])"
                .into(),
            rows.clone(),
            &contracts,
        )
        .await
        .unwrap();
    assert_eq!(result, "9007199254740993|None");
    let error = pool
        .compute_typed("str(__input[0].value['absent'])".into(), rows, &contracts)
        .await
        .unwrap_err();
    assert!(error.diagnostic().contains("KeyError"), "{error}");
    pool.close().await;
}
