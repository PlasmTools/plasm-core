//! Python expression coverage is specified by live semantic witnesses.
#[tokio::test]
async fn literal_contracts_require_live_witnesses_and_rejections() {
    super::constructor_evidence::assert_expression_contracts(
        include_str!("../../../../doc-site/docs/reference/python-literal-constructors.md"),
        "plasm-literal-constructors",
        &[
            ("text", &["operand_recursive_literals"]),
            ("number", &["operand_recursive_literals"]),
            ("signed", &["operand_recursive_literals"]),
            ("boolean", &["operand_recursive_literals"]),
            ("null", &["operand_recursive_literals"]),
            ("array", &["operand_recursive_literals"]),
            ("record", &["operand_recursive_literals"]),
        ],
    )
    .await;
}
