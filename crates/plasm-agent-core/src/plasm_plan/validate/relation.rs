use super::*;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlanRelationValidationError {
    #[error("plan node {node_index} relation source `{source_id}` is unknown")]
    UnknownSource {
        node_index: usize,
        source_id: String,
    },
    #[error("plan node {node_index} relation name is invalid")]
    InvalidRelationName {
        node_index: usize,
        source: PlanAtomError,
    },
    #[error("plan node {node_index} relation target requires non-empty catalog and entity names")]
    InvalidTarget { node_index: usize },
    #[error(transparent)]
    Operand(#[from] compute::OperandValidationError),
    #[error(
        "plan node {node_index} relation source `{source_id}` is not statically singleton; use Plan.singleton(...) for runtime-checked traversal"
    )]
    SourceNotStaticallySingleton {
        node_index: usize,
        source_id: String,
    },
    #[error(transparent)]
    ViewEmbedProof(#[from] plasm_core::ViewEmbedProofError),
    #[error("plan node {node_index} relation source `{source_id}` is not view-produced by node `{producer}` for view `{view}`")]
    SourceDoesNotReachProducer {
        node_index: usize,
        source_id: String,
        producer: String,
        view: String,
    },
    #[error(
        "plan node {node_index} view producer `{producer}` must be a surface root (got {kind:?})"
    )]
    ProducerIsNotSurfaceRoot {
        node_index: usize,
        producer: String,
        kind: PlanNodeKind,
    },
}

pub(super) fn validate_relation_traversal(
    plan: &Plan,
    relation: &PlanRelationTraversal,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), PlanRelationValidationError> {
    if relation.source.trim().is_empty() || !by_id.contains_key(&relation.source) {
        return Err(PlanRelationValidationError::UnknownSource {
            node_index,
            source_id: relation.source.clone(),
        });
    }
    RelationName::new(relation.relation.clone()).map_err(|source| {
        PlanRelationValidationError::InvalidRelationName { node_index, source }
    })?;
    if relation.target.entry_id.trim().is_empty() || relation.target.entity.trim().is_empty() {
        return Err(PlanRelationValidationError::InvalidTarget { node_index });
    }
    let input_aliases: Vec<_> = plan.nodes[node_index]
        .uses_result
        .iter()
        .map(|input| (input.r#as.as_str(), input.node.as_str()))
        .collect();
    validate_expression_operands(
        &relation.ir.expr,
        node_index,
        &plasm_core::TemplateRefContext {
            row_binding: None,
            input_aliases: &input_aliases,
        },
    )?;
    // A one-cardinality relation over a plural source is a valid 1:1 flat-map (one target per
    // parent → a list aligned with the parents); it lowers to per-row fanout exactly like the
    // many-relation case. `Plan.singleton(...)` is a narrowing assertion, never a prerequisite for
    // traversal. See the cardinality lattice in `docs/plasm-language-definition.md`.
    if relation.cardinality == RelationCardinality::One
        && relation.source_cardinality == RelationSourceCardinality::Single
        && !cardinality::analyze_static_cardinality(plan, by_id, relation.source.as_str())
            .is_static_singleton()
    {
        return Err(PlanRelationValidationError::SourceNotStaticallySingleton {
            node_index,
            source_id: relation.source.clone(),
        });
    }
    validate_view_embed_relation(plan, relation, node_index, by_id)?;
    Ok(())
}

fn validate_view_embed_relation(
    plan: &Plan,
    relation: &PlanRelationTraversal,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), PlanRelationValidationError> {
    let context = format!("plan.nodes[{node_index}]");
    let Some(proof) = plasm_core::ValidatedViewEmbedProof::require_for_materialize(
        relation.materialize.as_ref(),
        relation.view_embed_proof.as_ref(),
        relation.relation.as_str(),
        context.as_str(),
    )?
    else {
        return Ok(());
    };
    proof.ensure_producer_known(|id| by_id.contains_key(id), context.as_str())?;
    if !plan_node_reaches_view_producer(
        plan,
        by_id,
        relation.source.as_str(),
        proof.producer_node.as_str(),
    ) {
        return Err(PlanRelationValidationError::SourceDoesNotReachProducer {
            node_index,
            source_id: relation.source.clone(),
            producer: proof.producer_node.clone(),
            view: proof.view.clone(),
        });
    }
    let producer_idx = by_id[proof.producer_node.as_str()];
    let producer = &plan.nodes[producer_idx];
    if producer.kind.has_surface_expr() {
        return Ok(());
    }
    Err(PlanRelationValidationError::ProducerIsNotSurfaceRoot {
        node_index,
        producer: proof.producer_node.clone(),
        kind: producer.kind,
    })
}

fn plan_node_reaches_view_producer(
    plan: &Plan,
    by_id: &HashMap<String, usize>,
    start: &str,
    producer: &str,
) -> bool {
    let mut cur = start.to_string();
    let mut visited = std::collections::HashSet::new();
    for _ in 0..64 {
        if cur == producer {
            return true;
        }
        if !visited.insert(cur.clone()) {
            return false;
        }
        let Some(idx) = by_id.get(cur.as_str()) else {
            return false;
        };
        let node = &plan.nodes[*idx];
        cur = match node.kind {
            PlanNodeKind::Relation => node
                .relation
                .as_ref()
                .map(|r| r.source.clone())
                .unwrap_or_default(),
            PlanNodeKind::Compute => node
                .compute
                .as_ref()
                .map(|c| c.source.clone())
                .unwrap_or_default(),
            PlanNodeKind::Derive => node
                .derive_template
                .as_ref()
                .and_then(|d| d.source.clone())
                .unwrap_or_default(),
            PlanNodeKind::ForEach | PlanNodeKind::IterateUntil => {
                node.source.clone().unwrap_or_default()
            }
            PlanNodeKind::MapBody => match node.map_body.as_deref() {
                Some(body)
                    if matches!(body.output, plasm_core::plasm_monad::ScopedOutput::Filter) =>
                {
                    body.parent.source.to_string()
                }
                _ => return false,
            },
            PlanNodeKind::Data => return false,
            _ => return false,
        };
    }
    false
}
