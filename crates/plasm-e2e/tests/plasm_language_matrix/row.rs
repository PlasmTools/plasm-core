//! One language-matrix conformance row.

#[derive(Clone, Copy)]
pub(crate) struct MatrixRow {
    pub id: &'static str,
    pub program: &'static str,
    /// Use `compile_plasm_expression` for this row (single expression / comma roots).
    pub surface_line: bool,
    /// Federated session: primary `langmatrix` + secondary views catalog.
    pub federated: bool,
    pub features: &'static [&'static str],
    /// Minimum `PlasmPlanRunResult::node_results` length after live run.
    pub min_node_results: usize,
    /// Each substring must appear in `PlasmPlanRunResult::run_markdown`.
    pub expect_markdown_substrings: &'static [&'static str],
    /// When set, live execute must fail containing this substring (e.g. PLP-8 bound exhaustion).
    pub expect_live_error: Option<&'static str>,
}
