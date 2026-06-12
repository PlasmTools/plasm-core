use plasm_core::expr_parser::ParsedExpr;

/// One materialized return-step run snapshot for `run_sealed` recording.
#[derive(Debug, Clone)]
pub struct RunSealRecord {
    pub expected_run_id_wire: String,
    pub step_id: Option<String>,
    pub resource_index: Option<u64>,
    pub entry_id: String,
    pub source_line: String,
    pub parsed: ParsedExpr,
    pub request_fingerprints: Vec<String>,
}

/// One materialized plan step for batched `step_executed` recording.
#[derive(Debug, Clone)]
pub struct StepExecutedRecord {
    pub step_id: String,
    pub step_index: u32,
    pub entry_id: Option<String>,
    pub source_line: String,
    pub parsed: ParsedExpr,
    pub request_fingerprints: Vec<String>,
}
