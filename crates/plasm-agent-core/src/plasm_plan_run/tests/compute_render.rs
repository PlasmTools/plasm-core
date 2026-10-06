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
    rows: &[plasm_core::ValueRow],
    cols: &RenderColumns,
    template: &str,
    collection_alias: Option<&OutputName>,
) -> Result<Vec<plasm_core::ValueRow>, super::super::compute_eval::RenderComputeError> {
    render_compute(&RenderComputeInput {
        output_name: "content",
        primary_rows: rows,
        columns: cols,
        template,
        collection_alias,
        render_bindings: &[],
        binding_rows: &BTreeMap::new(),
    })
}

#[test]
fn render_compute_emits_one_content_record_per_row() {
    let rows = vec![
        crate::fixture_row!({ "name": "a" }),
        crate::fixture_row!({ "name": "b" }),
    ];
    let cols = empty_cols(&["name"]);
    let out = render(&rows, &cols, "{{ name }}", None).expect("render");

    assert_eq!(
        out,
        vec![
            crate::fixture_row!({ "content": "a" }),
            crate::fixture_row!({ "content": "b" }),
        ]
    );
}

#[test]
fn render_compute_zero_rows_emits_empty_rowset() {
    let cols = empty_cols(&["name"]);
    let out = render(&[], &cols, "{{ name }}", None).expect("empty");
    assert!(out.is_empty());
}

#[test]
fn render_compute_named_binding_available_without_implicit_rows() {
    let rows = vec![
        crate::fixture_row!({ "name": "a" }),
        crate::fixture_row!({ "name": "b" }),
    ];
    let cols = empty_cols(&["name"]);
    let items = OutputName::new("items").expect("alias");
    let out = render_compute(&RenderComputeInput {
        output_name: "content",
        primary_rows: &rows,
        columns: &cols,
        template: "{{ name }} ({{ items | length }})",
        collection_alias: Some(&items),
        render_bindings: std::slice::from_ref(&items),
        binding_rows: &BTreeMap::from([("items".to_string(), rows.clone())]),
    })
    .expect("named binding");

    assert_eq!(
        out,
        vec![
            crate::fixture_row!({ "content": "a (2)" }),
            crate::fixture_row!({ "content": "b (2)" }),
        ]
    );
}

#[test]
fn render_compute_p_symbol_alias_resolves_alongside_wire_name() {
    let rows = vec![
        crate::fixture_row!({ "name": "a", "id": 1 }),
        crate::fixture_row!({ "name": "b", "id": 2 }),
    ];
    let mut aliases = BTreeMap::new();
    aliases.insert("p23".into(), OutputName::new("name").expect("name"));
    aliases.insert("p21".into(), OutputName::new("id").expect("id"));
    let cols = render_cols(&["name", "id"], aliases);
    let out = render(&rows, &cols, "{{ p23 }} (#{{ p21 }})", None).expect("p# aliases");

    assert_eq!(
        out,
        vec![
            crate::fixture_row!({ "content": "a (#1)" }),
            crate::fixture_row!({ "content": "b (#2)" }),
        ]
    );
}

#[test]
fn render_compute_null_field_coalesces_with_or() {
    let rows = vec![
        crate::fixture_row!({ "name": "a", "score": null }),
        crate::fixture_row!({ "name": "b", "score": 42 }),
    ];
    let cols = empty_cols(&["name", "score"]);
    let out =
        render(&rows, &cols, "{{ name }}: {{ score or \"—\" }}", None).expect("null coalesce");
    assert_eq!(
        out,
        vec![
            crate::fixture_row!({ "content": "a: —" }),
            crate::fixture_row!({ "content": "b: 42" }),
        ]
    );
}

#[test]
fn render_compute_split_part_filter_matches_taught_minijinja() {
    let rows = vec![crate::fixture_row!({ "blob": "alpha:beta:gamma" })];
    let cols = empty_cols(&["blob"]);
    let out = render(&rows, &cols, "{{ blob | split_part(':', 1) }}", None)
        .expect("split_part is a shared Minijinja filter");
    assert_eq!(out, vec![crate::fixture_row!({ "content": "beta" })]);
}

#[test]
fn render_compute_propagates_minijinja_errors_with_row_position() {
    let rows = vec![crate::fixture_row!({ "name": "a" })];
    let cols = empty_cols(&["name"]);
    let err = render(&rows, &cols, "{{ missing }}", None).expect_err("strict undefined");

    assert!(matches!(
        &err,
        super::super::compute_eval::RenderComputeError::Template { row_index: 0, .. }
    ));
    assert!(
        !err.to_string().contains("\"a\""),
        "must not expose row values: {err}"
    );
}

#[test]
fn render_compute_fails_closed_on_first_row_error() {
    let rows = vec![
        crate::fixture_row!({ "name": "a" }),
        crate::fixture_row!({ "other": "b" }),
    ];
    let cols = empty_cols(&[]);
    let err = render(&rows, &cols, "{{ name }}", None).expect_err("row 1 missing name");
    assert!(matches!(
        err,
        super::super::compute_eval::RenderComputeError::Template { row_index: 1, .. }
    ));
}

#[test]
fn render_compute_preserves_unicode_and_whitespace() {
    let rows = vec![crate::fixture_row!({
        "title": "Pokémon",
        "arrow": "→",
    })];
    let cols = empty_cols(&["title", "arrow"]);
    let rendered =
        render(&rows, &cols, "# {{ title }}\nstep {{ arrow }} done", None).expect("render unicode");
    let content = rendered[0]["content"].as_str().expect("content");
    assert!(content.contains("Pokémon"), "{content}");
    assert!(content.contains('→'), "{content}");
    assert!(content.contains("# "), "{content}");
}

#[test]
fn render_compute_no_implicit_rows_variable() {
    let rows = vec![crate::fixture_row!({ "name": "a" })];
    let cols = empty_cols(&["name"]);
    let err = render(&rows, &cols, "{{ rows | length }}", None).expect_err("implicit rows is gone");
    assert!(matches!(
        err,
        super::super::compute_eval::RenderComputeError::Template { row_index: 0, .. }
    ));
}

#[test]
fn render_compute_matrix_sized_rows_within_wall_time_guard() {
    let started = std::time::Instant::now();
    let rows: Vec<_> = (0..100)
        .map(|i| crate::fixture_row!({ "id": format!("i{i}"), "title": format!("t{i}") }))
        .collect();
    let cols = empty_cols(&["id", "title"]);
    let out = render(&rows, &cols, "{{ id }}", None).expect("render");
    assert_eq!(out.len(), 100);
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_millis() < 500,
        "per-row render on 100 rows should stay sub-second, took {elapsed:?}"
    );
}
