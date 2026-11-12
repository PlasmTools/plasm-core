//! Result provenance from validated nodes, never from row values or Python text.

use super::{ComputeOp, ValidatedPlanNode};

pub(super) fn result_heading(returned: &str, nodes: &[ValidatedPlanNode]) -> Option<String> {
    let mut current = returned;
    let mut binding = None;
    let mut projected = false;
    // A validated DAG is acyclic; bound traversal defensively without guessing.
    for _ in 0..nodes.len() {
        let node = nodes.iter().find(|node| node.id().as_str() == current)?;
        let is_projection = matches!(node, ValidatedPlanNode::Compute(compute) if matches!(compute.compute.op, ComputeOp::Project { .. }));
        if binding.is_none() && !is_projection {
            binding = crate::agent_presentation::source_binding_label(current);
        }
        match node {
            ValidatedPlanNode::Surface(source) => {
                let source_type = source.qualified_entity.as_ref()?;
                let binding = binding.unwrap_or("Result");
                let kind = if projected { "projection from " } else { "" };
                let label = format!(
                    "{kind}{binding}: {}::{}",
                    source_type.entry_id, source_type.entity
                );
                return Some(
                    match crate::agent_presentation::source_binding_label(returned) {
                        Some(output) if projected && output != binding => {
                            format!("{output} — {label}")
                        }
                        _ => label,
                    },
                );
            }
            ValidatedPlanNode::Compute(compute) => {
                match &compute.compute.op {
                    ComputeOp::Project { .. } => projected = true,
                    ComputeOp::Filter { .. }
                    | ComputeOp::Sort { .. }
                    | ComputeOp::Limit { .. }
                    | ComputeOp::DedupeBy { .. } => {}
                    // Joins, arbitrary Python, aggregates and unions must not
                    // acquire a single source type merely from one dependency.
                    _ => return None,
                }
                current = compute.source_node.as_str();
            }
            _ => return None,
        }
    }
    None
}
