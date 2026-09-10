use super::*;

pub(super) fn validate_relation_traversal(
    plan: &Plan,
    relation: &PlanRelationTraversal,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), String> {
    if relation.source.trim().is_empty() || !by_id.contains_key(&relation.source) {
        return Err(format!(
            "plan.nodes[{node_index}].relation.source references unknown id {:?}",
            relation.source
        ));
    }
    RelationName::new(relation.relation.clone())
        .map_err(|e| format!("plan.nodes[{node_index}].relation.relation: {e}"))?;
    if relation.target.entry_id.trim().is_empty() || relation.target.entity.trim().is_empty() {
        return Err(format!(
            "plan.nodes[{node_index}].relation.target must include non-empty entry_id and entity"
        ));
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
    // A one-cardinality relation over a *plural* source is a valid 1:1 flat-map (one target per
    // parent → a list aligned with the parents); it lowers to per-row fanout exactly like the
    // many-relation case. `Plan.singleton(...)` is a narrowing assertion, never a prerequisite for
    // traversal. See the cardinality lattice in `docs/plasm-language-definition.md`.
    if relation.cardinality == RelationCardinality::One
        && relation.source_cardinality == RelationSourceCardinality::Single
        && !cardinality::analyze_static_cardinality(plan, by_id, relation.source.as_str())
            .is_static_singleton()
    {
        return Err(format!(
            "plan.nodes[{node_index}].relation source {:?} is not statically singleton; use Plan.singleton(...) for runtime-checked traversal",
            relation.source
        ));
    }
    validate_view_embed_relation(plan, relation, node_index, by_id)?;
    Ok(())
}

fn validate_view_embed_relation(
    plan: &Plan,
    relation: &PlanRelationTraversal,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<(), String> {
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
    )? {
        return Err(format!(
            "plan.nodes[{node_index}].relation source {:?} is not view-produced by node {:?} for view `{}`",
            relation.source, proof.producer_node, proof.view
        ));
    }
    let producer_idx = by_id[proof.producer_node.as_str()];
    let producer = &plan.nodes[producer_idx];
    if producer.kind.has_surface_expr() {
        return Ok(());
    }
    Err(format!(
        "plan.nodes[{node_index}].relation.view_embed_proof.producer_node {:?} must be a surface view root (got {:?})",
        proof.producer_node, producer.kind
    ))
}

fn plan_node_reaches_view_producer(
    plan: &Plan,
    by_id: &HashMap<String, usize>,
    start: &str,
    producer: &str,
) -> Result<bool, String> {
    let mut cur = start.to_string();
    let mut visited = std::collections::HashSet::new();
    for _ in 0..64 {
        if cur == producer {
            return Ok(true);
        }
        if !visited.insert(cur.clone()) {
            return Ok(false);
        }
        let Some(idx) = by_id.get(cur.as_str()) else {
            return Ok(false);
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
            PlanNodeKind::Data => return Ok(false),
            _ => return Ok(false),
        };
    }
    Ok(false)
}
