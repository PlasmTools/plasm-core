//! Bound read operands exercised identically by both source frontends.
use super::MatrixRow;
pub(crate) const ROWS: &[MatrixRow] = &[
    MatrixRow {
        id: "lang_bound_get_field",
        federated: false,
        features: &["bound_get_identity", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bound_get_scalar",
        federated: false,
        features: &["bound_get_identity", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bound_query_field",
        federated: false,
        features: &["bound_query_operand", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bound_query_scalar",
        federated: false,
        features: &["bound_query_operand", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "owner"],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_iterate_bound_identity",
        federated: false,
        features: &["bound_iterate_identity", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &["```tsv", "phase", "/langcursor_tick`: 2 completed."],
        expect_live_error: None,
    },
    MatrixRow {
        id: "lang_bound_get_empty",
        federated: false,
        features: &["bound_get_identity", "dry_live_parity"],
        min_node_results: 2,
        expect_markdown_substrings: &[],
        expect_live_error: Some("zero rows"),
    },
];
