use std::collections::BTreeMap;

use crate::plasm_plan::OutputName;

use super::super::*;

fn render_cols(wires: &[&str], aliases: BTreeMap<String, OutputName>) -> RenderColumns {
    RenderColumns::from_op_parts(
        wires
            .iter()
            .map(|w| OutputName::new(*w).expect("column"))
            .collect(),
        aliases,
    )
}

fn empty_cols(wires: &[&str]) -> RenderColumns {
    render_cols(wires, BTreeMap::new())
}

fn render(
    rows: &[serde_json::Value],
    cols: &RenderColumns,
    template: &str,
    collection_alias: Option<&OutputName>,
) -> Result<Vec<serde_json::Value>, String> {
    render_compute(rows, cols, template, collection_alias)
}

#[test]
fn render_compute_emits_single_content_row() {
    let rows = vec![
        serde_json::json!({ "name": "a" }),
        serde_json::json!({ "name": "b" }),
    ];
    let cols = empty_cols(&["name"]);
    let out = render(
        &rows,
        &cols,
        "{% for r in rows %}- {{ r.name }}\n{% endfor %}",
        None,
    )
    .expect("render");

    assert_eq!(out, vec![serde_json::json!({ "content": "- a\n- b\n" })]);
}

#[test]
fn render_compute_source_alias_binds_collection_under_label() {
    let rows = vec![
        serde_json::json!({ "name": "a" }),
        serde_json::json!({ "name": "b" }),
    ];
    let cols = empty_cols(&["name"]);
    let alias = OutputName::new("items").expect("alias");
    let out = render(
        &rows,
        &cols,
        "{% for r in items %}- {{ r.name }}\n{% endfor %}",
        Some(&alias),
    )
    .expect("render with source alias");

    assert_eq!(out, vec![serde_json::json!({ "content": "- a\n- b\n" })]);
}

#[test]
fn render_compute_p_symbol_alias_resolves_alongside_wire_name() {
    let rows = vec![
        serde_json::json!({ "name": "a", "id": 1 }),
        serde_json::json!({ "name": "b", "id": 2 }),
    ];
    let mut aliases = BTreeMap::new();
    aliases.insert("p23".into(), OutputName::new("name").expect("name"));
    aliases.insert("p21".into(), OutputName::new("id").expect("id"));
    let cols = render_cols(&["name", "id"], aliases);
    let out = render(
        &rows,
        &cols,
        "{% for r in rows %}- {{ r.p23 }} (#{{ r.p21 }})\n{% endfor %}",
        None,
    )
    .expect("render p# aliases");

    assert_eq!(
        out,
        vec![serde_json::json!({ "content": "- a (#1)\n- b (#2)\n" })]
    );
}

#[test]
fn render_compute_mixed_p_and_wire_names() {
    let rows = vec![serde_json::json!({ "name": "a" })];
    let mut aliases = BTreeMap::new();
    aliases.insert("p23".into(), OutputName::new("name").expect("name"));
    let cols = render_cols(&["name"], aliases);
    let out = render(
        &rows,
        &cols,
        "{% for r in rows %}{{ r.p23 }} / {{ r.name }}{% endfor %}",
        None,
    )
    .expect("mixed");
    assert_eq!(out, vec![serde_json::json!({ "content": "a / a" })]);
}

#[test]
fn render_compute_null_field_coalesces_with_or() {
    let rows = vec![
        serde_json::json!({ "name": "a", "score": null }),
        serde_json::json!({ "name": "b", "score": 42 }),
    ];
    let cols = empty_cols(&["name", "score"]);
    let out = render(
        &rows,
        &cols,
        "{% for r in rows %}{{ r.name }}: {{ r.score or \"—\" }}\n{% endfor %}",
        None,
    )
    .expect("null coalesce");
    assert_eq!(out, vec![serde_json::json!({ "content": "a: —\nb: 42\n" })]);
}

#[test]
fn render_compute_null_renders_none_literal_without_coalesce() {
    let rows = vec![serde_json::json!({ "name": "a", "score": null })];
    let cols = empty_cols(&["name", "score"]);
    let out = render(
        &rows,
        &cols,
        "{% for r in rows %}{{ r.name }}:{{ r.score }}{% endfor %}",
        None,
    )
    .expect("null bare");
    assert_eq!(out, vec![serde_json::json!({ "content": "a:none" })]);
}

#[test]
fn render_compute_propagates_minijinja_errors_with_field_hint() {
    let rows = vec![serde_json::json!({ "name": "a" })];
    let cols = empty_cols(&["name"]);
    let err =
        render(&rows, &cols, "{{ missing }}", None).expect_err("strict undefined is rejected");

    assert!(err.contains("Plan.render template render error"), "{err}");
    assert!(err.contains("Valid row fields: r.name"), "{err}");
    assert!(err.contains("{% for r in rows %}"), "{err}");
}

#[test]
fn render_compute_rejects_missing_columns_with_hint() {
    let rows = vec![serde_json::json!({ "name": "a" })];
    let cols = empty_cols(&["missing"]);
    let err = render(&rows, &cols, "{{ rows }}", None).expect_err("missing column rejected");

    assert!(err.contains("did not resolve in source row 0"), "{err}");
    assert!(err.contains("Valid row fields:"), "{err}");
}

#[test]
fn render_compute_preserves_unicode_markdown() {
    let rows = vec![serde_json::json!({
        "title": "Pokémon",
        "arrow": "→",
    })];
    let cols = empty_cols(&["title", "arrow"]);
    let rendered = render(
        &rows,
        &cols,
        "# {{ rows[0].title }}\nstep {{ rows[0].arrow }} done",
        None,
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
    let cols = empty_cols(&["name"]);
    let rendered = render(
        &rows,
        &cols,
        "{% for r in rows %}- {{ r.name }}\n{% endfor %}",
        None,
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
