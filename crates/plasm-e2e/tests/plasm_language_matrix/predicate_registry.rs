//! Python expression coverage is specified by live semantic witnesses.
#[tokio::test]
async fn predicate_contracts_require_live_witnesses_and_rejections() {
    super::constructor_evidence::assert_expression_contracts(
        include_str!("../../../../doc-site/docs/reference/python-predicate-constructors.md"),
        "plasm-predicate-constructors",
        &[
            ("and", &["complete_boolean_literal", "predicate_truth_and"]),
            ("or", &["complete_boolean_rowset"]),
            ("not", &["complete_boolean_literal"]),
            ("comparison", &["where"]),
            (
                "scalar",
                &[
                    "where",
                    "predicate_truth_string",
                    "predicate_truth_empty",
                    "predicate_truth_null",
                    "predicate_truth_iteration",
                ],
            ),
            (
                "null_test",
                &["predicate_null_test", "predicate_null_filter"],
            ),
            ("contains", &["complete_predicate_contains"]),
            ("literal_membership", &["complete_boolean_literal"]),
            (
                "rowset_membership",
                &[
                    "inline_membership",
                    "where_in_rowset",
                    "where_not_in_rowset",
                ],
            ),
            ("any", &["predicate_any_relation", "predicate_truth_any"]),
            ("all", &["predicate_all_relation"]),
        ],
    )
    .await;
}
