//! Emit producer-owned abstract failure fixtures for downstream decoder tests.
use plasm_runtime::execution_failure::{
    ComputeInferenceDiagnostic, ExecutionFailure, FailureCause, ProgramRejectionDetail,
    ProgramRejectionEvidence, UnmaterializableNode,
};

fn contract() -> Vec<ExecutionFailure> {
    let mut details = vec![
        ProgramRejectionDetail::UnionColumnsDiffer {
            left: vec!["a".into()],
            right: vec!["b".into()],
        },
        ProgramRejectionDetail::ReservedBinding {
            name: "reserved".into(),
        },
        ProgramRejectionDetail::StaticIterationRequired,
        ProgramRejectionDetail::BuildMutation,
    ];
    for node in [
        None,
        Some(UnmaterializableNode::Any),
        Some(UnmaterializableNode::Unknown),
        Some(UnmaterializableNode::Unsupported),
        Some(UnmaterializableNode::Bytes),
        Some(UnmaterializableNode::Enum),
        Some(UnmaterializableNode::Protocol),
        Some(UnmaterializableNode::NewType),
        Some(UnmaterializableNode::Record),
        Some(UnmaterializableNode::TruthRefinement),
    ] {
        details.push(ProgramRejectionDetail::ComputeInference {
            diagnostics: vec![
                ComputeInferenceDiagnostic {
                    code: "abstract_type_error".into(),
                    unmaterializable_type: node,
                    message: "Return type cannot be materialized".into(),
                    compute_line: Some(3),
                    compute_column: Some(4),
                    source_excerpt: Some("return value".into()),
                },
                ComputeInferenceDiagnostic {
                    code: "abstract_unknown_location".into(),
                    unmaterializable_type: None,
                    message: "Missing type".into(),
                    compute_line: None,
                    compute_column: None,
                    source_excerpt: None,
                },
            ],
        });
    }
    let mut failures = Vec::new();
    for detail in details {
        // Exhaustive match makes added producer variants require fixture review.
        match &detail {
            ProgramRejectionDetail::ComputeInference { .. }
            | ProgramRejectionDetail::UnionColumnsDiffer { .. }
            | ProgramRejectionDetail::ReservedBinding { .. }
            | ProgramRejectionDetail::StaticIterationRequired
            | ProgramRejectionDetail::BuildMutation => {}
        }
        let mut failure =
            ExecutionFailure::new(FailureCause::Program, "fixture", "Repair the program");
        failure.program_rejection = Some(ProgramRejectionEvidence {
            category: "type".into(),
            span_offset: None,
            detail: Some(detail),
        });
        failures.push(failure);
    }
    let mut upstream = ExecutionFailure::new(FailureCause::Upstream, "fixture", "Tool failed");
    upstream.mcp_tool_result = Some(
        serde_json::json!({"isError":true,"content":[{"type":"text","text":"original error"}]}),
    );
    failures.push(upstream);
    failures
}

fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&contract()).expect("serialize contract fixtures")
    );
}

#[test]
fn downstream_fixture_matches_native_serialization() {
    let expected: serde_json::Value = serde_json::from_str(include_str!(
        "../../../packages/plasm-agent/scripts/fixtures/native-failure-contract.json"
    ))
    .unwrap();
    assert_eq!(serde_json::to_value(contract()).unwrap(), expected);
}
