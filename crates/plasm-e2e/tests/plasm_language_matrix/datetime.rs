//! Standard datetime syntax through admission, typed IL and actual Monty execution.
use plasm_agent::plasm_compile::compile_python_program;
use serde_json::{json, Value};

pub(super) async fn run(source: &str) -> Result<Value, String> {
    let (es, host, _) = super::recursive_values::fixture_context("http://127.0.0.1:1".into());
    let bundle = compile_python_program(&es, source)
        .await
        .map_err(|e| e.to_string())?;
    let dry = super::evaluate_plasm_comp_dry(&es, &bundle)?;
    super::assert_comp_witness(&dry)?;
    let run = plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &es,
        &host,
        &es.prompt_hash,
        "datetime",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .map_err(|e| e.diagnostic().to_owned())?;
    Ok(serde_json::to_value(&run.return_steps[0].result.entities()[0].fields).unwrap())
}

#[tokio::test]
async fn datetime_outer_expression_matrix() {
    let cases = [
        ("datetime.fromisoformat('2024-01-01T01:00:00+01:00') in [datetime(2024, 1, 1, tzinfo=timezone.utc)]", json!(true)),
        ("[datetime.fromisoformat('2024-01-01T01:00:00+01:00')] == [datetime(2024, 1, 1, tzinfo=timezone.utc)]", json!(true)),
        ("(-timedelta(hours=1)).total_seconds()", json!(-3600.0)),
        ("datetime(2024, 1, 1, fold=0).isoformat()", json!("2024-01-01T00:00:00")),
        ("date(2024, 2, 29).isoformat()", json!("2024-02-29")),
        ("(date(2024, 2, 28) + timedelta(days=1)).isoformat()", json!("2024-02-29")),
        ("(datetime(2024, 1, 2, tzinfo=timezone.utc) - datetime(2024, 1, 1, tzinfo=timezone.utc)).total_seconds()", json!(86400.0)),
        ("datetime.fromisoformat('2024-01-01T01:00:00+01:00') == datetime(2024, 1, 1, tzinfo=timezone.utc)", json!(true)),
        ("datetime(2024, 2, 29, tzinfo=timezone.utc).date().day", json!(29)),
        ("timedelta(hours=-1).total_seconds()", json!(-3600.0)),
        ("date(2024, 1, 1).weekday()", json!(0)),
        ("datetime(2024, 1, 1, tzinfo=timezone.utc).strftime('%Y-%m-%d')", json!("2024-01-01")),
    ];
    for (expression, expected) in cases {
        let source = format!("from datetime import date, datetime, timedelta, timezone\nclass Temporal(Program):\n    def build(self):\n        return {{'result': {expression}}}\n");
        assert_eq!(
            run(&source)
                .await
                .unwrap_or_else(|e| panic!("{expression}: {e}")),
            json!({"result":expected}),
            "{expression}"
        );
    }
}

#[tokio::test]
async fn datetime_import_aliases_and_typed_compute_returns() {
    let source = "import datetime as dt\nclass Temporal(Program):\n    @compute\n    def shift(self, row: Row) -> dt.datetime:\n        return row.instant + dt.timedelta(days=1)\n    @compute\n    def render(self, row: Row) -> str:\n        return row.shifted.isoformat()\n    def build(self):\n        start = {'instant': dt.datetime(2024, 2, 28, tzinfo=dt.timezone.utc)}\n        next_day = self.shift(start)\n        return self.render({'shifted': next_day})\n";
    assert_eq!(
        run(source).await.unwrap(),
        json!({"value":"2024-02-29T00:00:00+00:00"})
    );
}

#[tokio::test]
async fn datetime_invalid_programs_fail_before_execution() {
    for expression in [
        "date(2024, 1, 1) + date(2024, 1, 2)",
        "datetime.fromtimestamp(0)",
        "timedelta(months=1)",
        "datetime(2024, 1, 1, fold=1)",
    ] {
        let source = format!("from datetime import date, datetime, timedelta\nclass Temporal(Program):\n    def build(self):\n        return {{'value': {expression}}}\n");
        let (es, _, _) = super::recursive_values::fixture_context("http://127.0.0.1:1".into());
        assert!(
            compile_python_program(&es, &source).await.is_err(),
            "admitted {expression}"
        );
    }
}

#[tokio::test]
async fn datetime_catalog_consumers_preserve_types_and_wire_units() {
    for enabled in [true, false] {
        catalog_consumers_with_nullable_cutoff(enabled).await;
    }
}

async fn catalog_consumers_with_nullable_cutoff(enabled: bool) {
    use std::sync::{Arc, Mutex};
    let written = Arc::new(Mutex::new(Vec::<Value>::new()));
    let sink = written.clone();
    let record = super::python::value_contract_matrix::record();
    let app = axum::Router::new()
        .route(
            "/samples/{id}",
            axum::routing::get(move || {
                let record = record.clone();
                async move { axum::Json(record) }
            }),
        )
        .route(
            "/samples/{id}/times",
            axum::routing::patch(move |axum::Json(mut body): axum::Json<Value>| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push(body.clone());
                    body["id"] = json!("000123");
                    axum::Json(body)
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (es, host, token) = super::recursive_values::fixture_context(base);
    let method = es
        .teaching_exposure
        .as_ref()
        .unwrap()
        .to_symbol_map()
        .method_sym_for("types", "Sample", "sample_update_times");
    let enabled_literal = if enabled { "True" } else { "False" };
    let source = format!("from datetime import datetime, timedelta, timezone\nclass Temporal(Program):\n    @compute\n    def optional_cutoff(self, row: Row) -> datetime | None:\n        return datetime(2026, 9, 25, tzinfo=timezone.utc) if row.enabled else None\n    def build(self):\n        row = {token}.get('000123')\n        cutoff = self.optional_cutoff({{'enabled': {enabled_literal}}})\n        filtered = row.where(lambda r: cutoff is None or r.timestamp > cutoff)\n        dates = filtered.select(next_date=lambda r: r.date + timedelta(days=1), same_instant=lambda r: r.epoch)\n        return row.{method}(date=dates.next_date, timestamp=cutoff if cutoff is not None else datetime(2026, 9, 25, tzinfo=timezone.utc), epoch=dates.same_instant)\n");
    let bundle = compile_python_program(&es, &source)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{source}"));
    let result = plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &es,
        &host,
        &es.prompt_hash,
        "datetime-consumers",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await;
    server.abort();
    result.unwrap();
    assert_eq!(
        *written.lock().unwrap(),
        vec![
            json!({"date":"2026-09-26", "timestamp":"2026-09-25T00:00:00+00:00", "epoch":1720000000123_i64})
        ]
    );
}

#[tokio::test]
async fn datetime_quantifiers_capture_typed_values() {
    let source = "from datetime import date, timedelta\nclass Temporal(Program):\n    def build(self):\n        days = [date(2024, 2, 28), date(2024, 2, 29)]\n        cutoff = date(2024, 2, 29)\n        return {'any': any(day >= cutoff for day in days), 'all': all(day + timedelta(days=1) >= cutoff for day in days)}\n";
    assert_eq!(run(source).await.unwrap(), json!({"any":true,"all":true}));
}

#[tokio::test]
async fn datetime_quantifiers_preserve_nested_scope_and_boolean_contracts() {
    for (expression, expected) in [
        ("any(day.year for day in [date(2024, 2, 29)])", true),
        ("any(day == day for day in [date(2024, 2, 29)] if day.year)", true),
        ("any(n == cutoff.year for n in [2024, 2025])", true),
        ("any(cutoff == 1 for cutoff in [1, 2])", true),
        ("all(any(cutoff == 1 for cutoff in [1, 2]) for cutoff in [3, 4])", true),
        ("any(cutoff.year == 2023 for cutoff in [date(2024, 1, 1)])", false),
        ("any(day >= cutoff for group in [[date(2024, 2, 28)], [date(2024, 2, 29)]] for day in group)", true),
        ("all(all(day <= cutoff for day in group) for group in [[date(2024, 2, 28)], [date(2024, 2, 29)]])", true),
        ("any(date.fromisoformat(text) >= cutoff for text in ['2024-02-28', '2024-02-29'])", true),
    ] {
        let source = format!("from datetime import date\nclass Temporal(Program):\n    def build(self):\n        cutoff = date(2024, 2, 29)\n        return {{'result': {expression}}}\n");
        assert_eq!(run(&source).await.unwrap_or_else(|e| panic!("{expression}: {e}")), json!({"result":expected}));
    }
    for expression in [
        "any(day.missing for day in [date(2024, 2, 29)])",
        "any(day == day for day in [date(2024, 2, 29)] if day.missing)",
    ] {
        let source = format!("from datetime import date\nclass Temporal(Program):\n    def build(self):\n        return {{'result': {expression}}}\n");
        let (es, _, _) = super::recursive_values::fixture_context("http://127.0.0.1:1".into());
        assert!(
            compile_python_program(&es, &source).await.is_err(),
            "{expression}"
        );
    }
}

#[tokio::test]
async fn datetime_iteration_reobserves_temporal_stop_predicate() {
    for enabled in [true, false] {
        iteration_with_nullable_deadline(enabled).await;
    }
}

async fn iteration_with_nullable_deadline(enabled: bool) {
    use std::sync::{Arc, Mutex};
    let state = Arc::new(Mutex::new(super::python::value_contract_matrix::record()));
    let reads = state.clone();
    let writes = state.clone();
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = count.clone();
    let app = axum::Router::new()
        .route(
            "/samples/{id}",
            axum::routing::get(move || {
                let state = reads.clone();
                async move { axum::Json(state.lock().unwrap().clone()) }
            }),
        )
        .route(
            "/samples/{id}/times",
            axum::routing::patch(move |axum::Json(body): axum::Json<Value>| {
                let state = writes.clone();
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let mut current = state.lock().unwrap();
                    current
                        .as_object_mut()
                        .unwrap()
                        .extend(body.as_object().unwrap().clone());
                    axum::Json(current.clone())
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (es, host, token) = super::recursive_values::fixture_context(format!(
        "http://{}",
        listener.local_addr().unwrap()
    ));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let method = es
        .teaching_exposure
        .as_ref()
        .unwrap()
        .to_symbol_map()
        .method_sym_for("types", "Sample", "sample_update_times");
    let enabled_literal = if enabled { "True" } else { "False" };
    let source = format!("from datetime import date, timedelta\nclass Temporal(Program):\n    @compute\n    def optional_deadline(self, row: Row) -> date | None:\n        return date(2026, 9, 27) if row.enabled else None\n    def build(self):\n        deadline = self.optional_deadline({{'enabled': {enabled_literal}}})\n        return {token}.get('000123').iterate(lambda r: r.{method}(date=r.date + timedelta(days=1), timestamp=r.timestamp, epoch=r.epoch), until=lambda r: deadline is None or r.date >= deadline, max_steps=3)\n");
    let bundle = compile_python_program(&es, &source)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{source}"));
    let dry = super::evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    super::assert_comp_witness(&dry).unwrap();
    for tamper in 0..3 {
        let mut comp = bundle.artifact().comp.clone();
        let unfold = comp
            .steps
            .values_mut()
            .find_map(|step| match step {
                plasm_core::PlasmStepPayload::UnfoldUntil(unfold) => Some(unfold),
                _ => None,
            })
            .unwrap();
        match tamper {
            0 => {
                unfold.step_scope.as_mut().unwrap().max_parents =
                    std::num::NonZeroU32::new(2).unwrap()
            }
            1 => unfold.effect_template.qualified_entity.entity = "Forged".into(),
            _ => {
                let body = &mut unfold.step_scope.as_mut().unwrap().body;
                body.steps.retain(|_, step| {
                    !matches!(
                        step.effect_class(),
                        plasm_core::plasm_monad::EffectClass::Write
                            | plasm_core::plasm_monad::EffectClass::SideEffect
                    )
                });
            }
        }
        assert!(
            comp.validate().is_err(),
            "accepted iteration tamper {tamper}"
        );
    }
    let result = plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &es,
        &host,
        &es.prompt_hash,
        "datetime-iteration",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await;
    server.abort();
    result.unwrap();
    assert_eq!(
        count.load(std::sync::atomic::Ordering::SeqCst),
        if enabled { 2 } else { 0 }
    );
    assert_eq!(
        state.lock().unwrap()["date"],
        json!(if enabled { "2026-09-27" } else { "2026-09-25" })
    );
}

fn nullable_program(present: bool, body: &str) -> String {
    format!("from datetime import date, datetime, timedelta, timezone\nclass Temporal(Program):\n    @compute\n    def optional(self, row: Row) -> datetime | None:\n        return datetime(2024, 1, 1, tzinfo=timezone.utc) if row.present else None\n    def build(self):\n        stamp = self.optional({{'present': {}}})\n        cutoff = datetime(2025, 1, 1, tzinfo=timezone.utc)\n{body}\n", if present { "True" } else { "False" })
}

#[tokio::test]
async fn datetime_nullable_guards_preserve_narrowing() {
    let cases = [
        ("any(value is not None and value < cutoff for value in [stamp])", json!(true), json!(false)),
        ("any(stamp is not None and stamp.year == 2024 for unused in [0])", json!(true), json!(false)),

        ("stamp is not None and stamp < cutoff", json!(true), json!(false)),
        ("stamp is None or stamp < cutoff", json!(true), json!(true)),
        ("not (stamp is None) and stamp.year == 2024", json!(true), json!(false)),
        ("stamp.year if stamp is not None else -1", json!(2024), json!(-1)),
        ("-1 if None is stamp else stamp.year", json!(2024), json!(-1)),
        ("stamp < cutoff if stamp != None else False", json!(true), json!(false)),
        ("(stamp + timedelta(days=1)).year if stamp is not None else -1", json!(2024), json!(-1)),
        ("stamp.date().isoformat() if stamp is not None else 'missing'", json!("2024-01-01"), json!("missing")),
        ("None is not stamp < cutoff", json!(true), json!(false)),
        ("stamp is not None and stamp.year == 2024 and stamp < cutoff", json!(true), json!(false)),
        ("stamp is None or (stamp.year == 2024 and stamp < cutoff)", json!(true), json!(true)),
        ("((stamp is not None and True) or (stamp is not None and False)) and stamp.year == 2024", json!(true), json!(false)),
    ];
    for (expression, yes, no) in cases {
        for (present, expected) in [(true, yes), (false, no)] {
            let source = nullable_program(
                present,
                &format!("        return {{'result': {expression}}}"),
            );
            assert_eq!(
                run(&source)
                    .await
                    .unwrap_or_else(|error| panic!("{expression}, present={present}: {error}")),
                json!({"result":expected})
            );
        }
    }
}

#[tokio::test]
async fn datetime_nullable_guards_cover_scoped_and_recursive_consumers() {
    for present in [true, false] {
        let year = if present { 2024 } else { -1 };
        let cases = [
            ("        row = {'stamp': stamp}\n        return {'value': any(row.stamp is not None and row.stamp < cutoff for unused in [0])}", json!({"value":present})),

            ("        row = {'stamp': stamp}\n        selected = row.where(lambda r: r.stamp is not None).select(value=lambda r: r.stamp.year)\n        return {'selected': selected}", json!({"selected": if present { json!([{"value":2024}]) } else { json!([]) }})),
            ("        row = {'stamp': stamp}\n        selected = row.where(lambda r: not (r.stamp is None)).select(value=lambda r: r.stamp.year)\n        return {'selected': selected}", json!({"selected": if present { json!([{"value":2024}]) } else { json!([]) }})),

            ("        alias = stamp\n        return {'value': alias.year if stamp is not None else -1}", json!({"value":year})),
            ("        alias = stamp\n        return {'value': stamp.year if (stamp is not None or alias is not None) else -1}", json!({"value":year})),

            ("        row = {'stamp': stamp}\n        return row.select(value=lambda r: any(r.stamp is not None and r.stamp < cutoff for unused in [0]))", json!({"value":present})),

            ("        row = {'stamp': stamp}\n        return row.select(value=lambda r: r.stamp.year if r.stamp is not None else -1)", json!({"value":year})),
            ("        row = {'stamp': stamp}\n        return row.map(lambda r: {'value': r.stamp.year if r.stamp is not None else -1}, max_parents=1)", json!({"value":year})),
            ("        row = {'stamp': stamp}\n        filtered = row.where(lambda r: r.stamp is not None and r.stamp < cutoff)\n        return filtered.aggregate(count=agg.count())", json!({"count":if present {1} else {0}})),
            ("        return {'nested': [{'year': stamp.year if stamp is not None else -1}]}", json!({"nested":[{"year":year}]})),
        ];
        for (body, expected) in cases {
            assert_eq!(
                run(&nullable_program(present, body))
                    .await
                    .unwrap_or_else(|error| panic!("{body}, present={present}: {error}")),
                expected
            );
        }
    }
}

#[tokio::test]
async fn datetime_nullable_guards_reject_unproved_or_leaked_facts() {
    let (es, _, _) = super::recursive_values::fixture_context("http://127.0.0.1:1".into());
    for expression in [
        "stamp.year",
        "stamp < cutoff",
        "stamp is not None or stamp.year == 2024",
        "stamp.year if stamp is None else -1",
        "stamp.year if (stamp is not None or True) else -1",
        "stamp.year if not (stamp is not None) else -1",
    ] {
        let source = nullable_program(true, &format!("        safe = {{'year': stamp.year if stamp is not None else -1}}\n        return {{'result': {expression}}}"));
        assert!(
            compile_python_program(&es, &source).await.is_err(),
            "admitted {expression}"
        );
    }
    for predicate in ["r.stamp is None", "r.stamp is not None or True"] {
        let source = nullable_program(true, &format!("        row = {{'stamp': stamp}}\n        return row.where(lambda r: {predicate}).select(value=lambda r: r.stamp.year)"));
        assert!(
            compile_python_program(&es, &source).await.is_err(),
            "over-refined {predicate}"
        );
    }
}

#[tokio::test]
async fn datetime_naive_response_is_preserved_through_projection_and_compute() {
    let mut record = super::python::value_contract_matrix::record();
    record["local_timestamp"] = json!("2023-02-28T12:34:56.123456");
    let app = axum::Router::new().route(
        "/samples/{id}",
        axum::routing::get(move || {
            let record = record.clone();
            async move { axum::Json(record) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (es, host, token) = super::recursive_values::fixture_context(base);
    let source = format!("from datetime import datetime, timedelta\nclass Temporal(Program):\n    @compute\n    def inspect(self, row: Row) -> str:\n        assert row.local_timestamp is not None\n        assert row.local_timestamp.tzinfo is None\n        return (row.local_timestamp + timedelta(days=1)).isoformat()\n    def build(self):\n        row = {token}.get('000123').select('local_timestamp')\n        return self.inspect(row)\n");
    let bundle = compile_python_program(&es, &source).await.unwrap();
    let result = plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &es,
        &host,
        &es.prompt_hash,
        "naive-date",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        result.return_steps[0].result.entities()[0].fields["value"].to_value(),
        plasm_core::Value::String("2023-03-01T12:34:56.123456".into())
    );
    server.abort();
}

#[tokio::test]
async fn synthetic_filter_chains_keep_row_scope_and_dag_semantics() {
    let source = "from datetime import date\nclass Filter(Program):\n    def build(self):\n        row = {'stamp': date(2024, 1, 1)}\n        selected = row.where(lambda r: r.stamp.year == 2024).select(value=lambda r: r.stamp.year)\n        return {'selected': selected}\n";
    assert_eq!(
        run(source).await.unwrap(),
        json!({"selected":[{"value":2024}]})
    );
}

#[tokio::test]
async fn branch_refinement_is_shared_across_value_domains_and_nested_fields() {
    for (annotation, value, predicate, expression, expected) in [
        (
            "int | None",
            "1 if True else None",
            "r.value is not None",
            "r.value + 2",
            json!(3),
        ),
        (
            "str | None",
            "'ok' if True else None",
            "not (r.value is None)",
            "r.value.upper()",
            json!("OK"),
        ),
        (
            "str | int | None",
            "'ok' if True else 7",
            "isinstance(r.value, str)",
            "r.value.upper()",
            json!("OK"),
        ),
    ] {
        let source = format!("class Refine(Program):\n    @compute\n    def source(self, row: Row) -> {annotation}:\n        return {value}\n    def build(self):\n        value = self.source({{'seed': 1}})\n        rows = {{'value': value}}\n        selected = rows.where(lambda r: {predicate}).select(result=lambda r: {expression})\n        return {{'selected': selected}}\n");
        // The declared union prevents the fixture value from supplying a proof.
        assert_eq!(
            run(&source).await.unwrap(),
            json!({"selected":[{"result":expected}]})
        );
    }
    let source = "class Nested(Program):\n    @compute\n    def source(self, row: Row) -> int | None:\n        return 4\n    def build(self):\n        value = self.source({'seed': 1})\n        rows = {'box': {'value': value}}\n        selected = rows.where(lambda r: r.box.value is not None).select(result=lambda r: r.box.value + 1)\n        return {'selected': selected}\n";
    assert_eq!(
        run(source).await.unwrap(),
        json!({"selected":[{"result":5}]})
    );
}

#[tokio::test]
async fn branch_refinement_preserves_union_evidence_in_lazy_compute_scopes() {
    for (value, expected) in [("'ok'", "OK"), ("7", "8")] {
        let source = format!("class Branch(Program):\n    @compute\n    def source(self, row: Row) -> str | int:\n        return {value}\n    @compute\n    def echo(self, row: Row) -> str:\n        return row.text\n    def build(self):\n        value = self.source({{'seed': 1}})\n        selected = self.echo({{'text': value.upper()}}) if isinstance(value, str) else self.echo({{'text': str(value + 1)}})\n        return {{'selected': selected}}\n");
        assert_eq!(run(&source).await.unwrap(), json!({"selected": expected}));
        let start = source.find("        selected = ").unwrap();
        let end = source[start..].find('\n').unwrap() + start;
        let rhs = source[start..end]
            .strip_prefix("        selected = ")
            .unwrap();
        let mut nested = source.clone();
        nested.replace_range(start..end, &format!("        selected = [{rhs}]"));
        assert_eq!(run(&nested).await.unwrap(), json!({"selected": [expected]}));
        for (expression, short_circuit) in [
            (
                "isinstance(value, str) and self.echo({'text': value.upper()})",
                false,
            ),
            (
                "not isinstance(value, str) or self.echo({'text': value.upper()})",
                true,
            ),
        ] {
            let mut boolean = source.clone();
            boolean.replace_range(start..end, &format!("        selected = {expression}"));
            let expected = if value == "'ok'" {
                json!("OK")
            } else {
                json!(short_circuit)
            };
            assert_eq!(run(&boolean).await.unwrap(), json!({"selected": expected}));
        }
        let (es, _, _) = super::recursive_values::fixture_context("http://127.0.0.1:1".into());
        let wrong_branch =
            source.replace("if isinstance(value, str)", "if not isinstance(value, str)");
        assert!(compile_python_program(&es, &wrong_branch).await.is_err());
    }
}

#[tokio::test]
async fn datetime_query_consumers_encode_named_temporal_domains() {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    let observed = Arc::new(Mutex::new(Vec::new()));
    let sink = observed.clone();
    let app = axum::Router::new().route(
        "/samples",
        axum::routing::get(
            move |axum::extract::Query(query): axum::extract::Query<BTreeMap<String, String>>| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push(query);
                    axum::Json(json!([]))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (es, host, token) = super::recursive_values::fixture_context(base);
    let source = format!("from datetime import date, datetime, timezone\nclass QueryTimes(Program):\n    def build(self):\n        return {token}.query(date=date(2024, 2, 29), timestamp=datetime(2024, 2, 29, 12, 30, tzinfo=timezone.utc), local_timestamp=datetime(2024, 2, 29, 12, 30), epoch=datetime(2024, 1, 1, tzinfo=timezone.utc))\n");
    let bundle = compile_python_program(&es, &source).await.unwrap();
    let result = plasm_agent::plasm_plan_run::run_plasm_comp_python(
        &es,
        &host,
        &es.prompt_hash,
        "query-times",
        &bundle,
        true,
        None,
        None,
        None,
        None,
    )
    .await;
    server.abort();
    result.unwrap_or_else(|e| panic!("{}", e.diagnostic()));
    assert_eq!(
        *observed.lock().unwrap(),
        vec![BTreeMap::from([
            ("date".into(), "2024-02-29".into()),
            ("timestamp".into(), "2024-02-29T12:30:00+00:00".into()),
            ("local_timestamp".into(), "2024-02-29T12:30:00".into()),
            ("epoch".into(), "1704067200000".into()),
        ])]
    );
}
