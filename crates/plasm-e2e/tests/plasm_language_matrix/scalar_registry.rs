//! Python expression coverage is specified by live semantic witnesses.
#[tokio::test]
async fn scalar_contracts_require_live_witnesses_and_rejections() {
    super::constructor_evidence::assert_expression_contracts(
        include_str!("../../../../doc-site/docs/reference/python-scalar-constructors.md"),
        "plasm-scalar-constructors",
        &[
            ("text", &["text_literal_binding", "text_literal_equals"]),
            ("format", &["scoped_flat_map_format"]),
            (
                "field",
                &[
                    "cert_bind_singleton_field_scalar",
                    "cert_get_singleton_field_scalar",
                    "bound_get_scalar_keyword",
                ],
            ),
        ],
    )
    .await;
}

#[tokio::test]
async fn literal_string_spellings_preserve_live_values() {
    for spelling in [
        r#""hello-" + "matrix\n""#,
        r#""hello-" "matrix\n""#,
        r#"r"hello-matrix" + "\n""#,
        "\"\"\"hello-matrix\n\"\"\"",
    ] {
        let source =
            format!("class Text(Program):\n    def build(self):\n        return {spelling}\n");
        assert_eq!(
            super::datetime::run(&source).await.unwrap(),
            serde_json::json!({"value":"hello-matrix\n"})
        );
    }
}
