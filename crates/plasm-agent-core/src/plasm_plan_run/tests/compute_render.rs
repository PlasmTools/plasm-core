use super::super::*;

#[test]
fn render_compute_emits_single_content_row() {
    let rows = vec![
        serde_json::json!({ "name": "a" }),
        serde_json::json!({ "name": "b" }),
    ];
    let columns = vec![OutputName::new("name").expect("column")];
    let out = render_compute(
        &rows,
        &columns,
        "{% for r in rows %}- {{ r.name }}\n{% endfor %}",
    )
    .expect("render");

    assert_eq!(out, vec![serde_json::json!({ "content": "- a\n- b\n" })]);
}

#[test]
fn render_compute_propagates_minijinja_errors() {
    let rows = vec![serde_json::json!({ "name": "a" })];
    let columns = vec![OutputName::new("name").expect("column")];
    let err =
        render_compute(&rows, &columns, "{{ missing }}").expect_err("strict undefined is rejected");

    assert!(err.contains("Plan.render template render error"), "{err}");
}

#[test]
fn render_compute_rejects_missing_columns() {
    let rows = vec![serde_json::json!({ "name": "a" })];
    let columns = vec![OutputName::new("missing").expect("column")];
    let err = render_compute(&rows, &columns, "{{ rows }}").expect_err("missing column rejected");

    assert!(err.contains("did not resolve in source row 0"), "{err}");
}

#[test]
fn render_compute_preserves_unicode_markdown() {
    let rows = vec![serde_json::json!({
        "title": "Pokémon",
        "arrow": "→",
    })];
    let columns = vec![
        OutputName::new("title").expect("title"),
        OutputName::new("arrow").expect("arrow"),
    ];
    let rendered = render_compute(
        &rows,
        &columns,
        "# {{ rows[0].title }}\nstep {{ rows[0].arrow }} done",
    )
    .expect("render unicode");
    let content = rendered[0]["content"].as_str().expect("content");
    assert!(content.contains("Pokémon"), "{content}");
    assert!(content.contains('→'), "{content}");
}

#[test]
fn render_compute_feeds_node_input_for_action_content() {
    let rows = vec![
        serde_json::json!({ "name": "a" }),
        serde_json::json!({ "name": "b" }),
    ];
    let columns = vec![OutputName::new("name").expect("column")];
    let rendered = render_compute(
        &rows,
        &columns,
        "{% for r in rows %}- {{ r.name }}\n{% endfor %}",
    )
    .expect("render");
    let input = rendered.into_iter().next().expect("singleton row");
    let value = PlanValue::Object {
        fields: BTreeMap::from([(
            "content".to_string(),
            PlanValue::NodeSymbol {
                node: "doc".to_string(),
                alias: "doc".to_string(),
                path: vec!["content".to_string()],
            },
        )]),
    };
    let inputs = BTreeMap::from([(
        InputAlias::new("doc".to_string()).expect("alias"),
        MaterializedInputRow {
            node: PlanNodeId::new("doc".to_string()).expect("node id"),
            proof: crate::plasm_plan::InputCardinalityProof::StaticSingleton,
            row: input,
            row_identity: None,
        },
    )]);
    let scope = EvalScope::Root {
        row: &serde_json::Value::Null,
    };
    let env = PlanEvalEnv {
        scope,
        inputs: InputEnv { rows: &inputs },
        wire_coercion: None,
    };
    let out = eval_plan_value(&value, &env).expect("eval");

    assert_eq!(out["content"], "- a\n- b\n");
}
