//! Python expression coverage is specified by live semantic witnesses.
#[tokio::test]
async fn projection_contracts_require_live_witnesses_and_rejections() {
    super::constructor_evidence::assert_expression_contracts(
        include_str!("../../../../doc-site/docs/reference/python-projection-constructors.md"),
        "plasm-projection-constructors",
        &[
            ("field", &["render_parity_lang_with_mul"]),
            ("literal", &["render_parity_lang_with_mul"]),
            (
                "arithmetic",
                &[
                    "render_parity_lang_with_mul",
                    "render_parity_lang_with_div",
                    "render_parity_lang_with_concat",
                ],
            ),
            ("length", &["render_parity_lang_with_when_len"]),
            ("conditional", &["render_parity_lang_with_when_len"]),
        ],
    )
    .await;
}
