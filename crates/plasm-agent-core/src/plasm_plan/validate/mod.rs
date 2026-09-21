//! Validation and typed conversion for Plan artifacts.

mod compute;
mod relation;
mod value;

use super::*;
use compute::*;
use relation::*;
use std::collections::{BTreeMap, HashMap};
use value::*;

/// Deserialize a program-shaped [`Plan`] from a JSON value (same IR shape as evaluation archives).
pub fn parse_plan_value(plan: &serde_json::Value) -> Result<Plan, String> {
    serde_json::from_value(plan.clone()).map_err(|e| format!("invalid serialized plan: {e}"))
}

/// Deserialize and validate a serialized plan JSON value (HTTP resolved-plan, MCP, CLI).
pub fn parse_and_validate_plan_json(plan: &serde_json::Value) -> Result<ValidatedPlan, String> {
    let plan_typed = parse_plan_value(plan)?;
    validate_plan_artifact(&plan_typed)
}

/// Parse and validate one program-shaped Plan.
pub fn validate_plan(plan: &Plan) -> Result<(), String> {
    validate_plan_artifact(plan).map(|_| ())
}

/// Parse and validate one program-shaped Plan, returning typed execution metadata.
pub fn validate_plan_artifact(plan: &Plan) -> Result<ValidatedPlan, String> {
    if plan.version != 1 {
        return Err(format!("unsupported Plan version: {}", plan.version));
    }
    if plan.nodes.is_empty() {
        return Err(
            "plan.nodes must be non-empty: a Plasm program must include at least one executable DAG node (taught `query` / `get` / search / relation forms per teaching table); a literal-only final roots line is not executable alone"
                .to_string(),
        );
    }
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for (i, n) in plan.nodes.iter().enumerate() {
        if n.id.trim().is_empty() {
            return Err(format!("plan.nodes[{i}].id is empty"));
        }
        if by_id.insert(n.id.clone(), i).is_some() {
            return Err(format!("duplicate plan node id: {}", n.id));
        }
        PlanNodeId::new(n.id.clone()).map_err(|e| format!("plan.nodes[{i}].id: {e}"))?;
    }

    for (i, n) in plan.nodes.iter().enumerate() {
        let mut aliases = std::collections::HashSet::new();
        for input in &n.uses_result {
            InputAlias::new(input.r#as.clone())?;
            if !aliases.insert(input.r#as.as_str()) {
                return Err(format!(
                    "plan.nodes[{i}].uses_result duplicate alias {:?}",
                    input.r#as
                ));
            }
        }
        if n.kind.has_surface_expr() {
            if n.ir.is_none() && n.ir_template.is_none() {
                return Err(format!(
                    "plan.nodes[{i}].ir or ir_template is required for executable node {:?}",
                    n.kind
                ));
            }
            if n.ir.is_some() && n.ir_template.is_some() {
                return Err(format!(
                    "plan.nodes[{i}] must not carry both ir and ir_template"
                ));
            }
            let input_aliases: Vec<_> = n
                .uses_result
                .iter()
                .map(|input| (input.r#as.as_str(), input.node.as_str()))
                .collect();
            let ctx = plasm_core::TemplateRefContext {
                row_binding: None,
                input_aliases: &input_aliases,
            };
            if let Some(ir) = &n.ir {
                validate_expression_operands(&ir.expr, i, &ctx)?;
            }
            if let Some(template) = &n.ir_template {
                validate_plan_expr_template(template, i, "ir_template")?;
                validate_expression_operands(&template.expr, i, &ctx)?;
            }
            if n.qualified_entity.is_none() && n.result_shape != ResultShape::Page {
                return Err(format!(
                    "plan.nodes[{i}].qualified_entity is required for executable node {:?}",
                    n.kind
                ));
            }
            if n.kind == PlanNodeKind::Search {
                if n.effect_class != EffectClass::Read {
                    return Err(format!("plan.nodes[{i}].search effect_class must be read"));
                }
                if n.result_shape != ResultShape::List {
                    return Err(format!("plan.nodes[{i}].search result_shape must be list"));
                }
            }
        }
        if n.kind == PlanNodeKind::Derive
            && (n.expr.is_some() || n.ir.is_some() || n.ir_template.is_some())
        {
            return Err(format!(
                "plan.nodes[{i}].derive must not carry expr, ir, or ir_template"
            ));
        }
        if n.kind == PlanNodeKind::Data {
            if n.data.is_none() {
                return Err(format!("plan.nodes[{i}].data is required for data nodes"));
            }
            if n.expr.is_some() || n.ir.is_some() || n.ir_template.is_some() {
                return Err(format!(
                    "plan.nodes[{i}].data must not carry expr, ir, or ir_template"
                ));
            }
        }
        if n.kind == PlanNodeKind::Compute {
            let compute = n
                .compute
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{i}].compute is required for compute nodes"))?;
            if n.expr.is_some()
                || n.ir.is_some()
                || n.ir_template.is_some()
                || n.data.is_some()
                || n.effect_template.is_some()
                || n.relation.is_some()
            {
                return Err(format!(
                    "plan.nodes[{i}].compute must not carry expr, ir, ir_template, data, effect_template, or relation"
                ));
            }
            validate_compute_template(compute, i, &by_id)?;
        } else if n.compute.is_some() {
            return Err(format!(
                "plan.nodes[{i}].compute is only valid for compute nodes"
            ));
        }
        if n.kind == PlanNodeKind::Relation {
            let relation = n.relation.as_ref().ok_or_else(|| {
                format!("plan.nodes[{i}].relation is required for relation nodes")
            })?;
            if n.effect_class != EffectClass::Read {
                return Err(format!(
                    "plan.nodes[{i}].relation effect_class must be read"
                ));
            }
            if !matches!(n.result_shape, ResultShape::List | ResultShape::Single) {
                return Err(format!(
                    "plan.nodes[{i}].relation result_shape must be list or single"
                ));
            }
            if n.expr.is_some()
                || n.ir.is_some()
                || n.ir_template.is_some()
                || n.data.is_some()
                || n.effect_template.is_some()
                || n.compute.is_some()
            {
                return Err(format!(
                    "plan.nodes[{i}].relation must not carry expr, ir, ir_template, data, effect_template, or compute"
                ));
            }
            validate_relation_traversal(plan, relation, i, &by_id)?;
        } else if n.relation.is_some() {
            return Err(format!(
                "plan.nodes[{i}].relation is only valid for relation nodes"
            ));
        }
        if let Some(data) = &n.data {
            validate_plan_value_expr(data, i, "data")?;
        }
        if let Some(derive_template) = &n.derive_template {
            validate_plan_value_expr(&derive_template.value, i, "derive_template.value")?;
            if derive_template.kind == DeriveKind::Map {
                let source = derive_template.source.as_deref().unwrap_or_default();
                if source.trim().is_empty() || !by_id.contains_key(source) {
                    return Err(format!(
                        "plan.nodes[{i}].derive_template.source references unknown id {source:?}"
                    ));
                }
                let binding = derive_template.item_binding.as_deref().unwrap_or_default();
                if binding.trim().is_empty() {
                    return Err(format!(
                        "plan.nodes[{i}].derive_template.item_binding is required for map"
                    ));
                }
            }
            for input in &derive_template.inputs {
                validate_plan_data_input(input, i, &by_id)?;
                if input.cardinality == InputCardinality::Auto
                    && !cardinality::analyze_static_cardinality(plan, &by_id, input.node.as_str())
                        .permits_auto_broadcast()
                {
                    return Err(format!(
                        "plan.nodes[{i}].derive_template.inputs node {:?} is not statically singleton; wrap it with Plan.singleton(...) to request runtime-checked broadcast",
                        input.node
                    ));
                }
            }
            validate_derive_value_inputs(derive_template, i)?;
        }
        for (j, p) in n.predicates.iter().enumerate() {
            validate_predicate(p, i, j)?;
        }
        if n.kind == PlanNodeKind::ForEach {
            let (binding, template) =
                validate_row_effect_source_and_template(n, i, &by_id, "for_each")?;
            for b in &template.input_bindings {
                if !b.from.starts_with(&format!("{binding}."))
                    && b.from.as_str() != binding
                    && !b.from.contains('.')
                {
                    return Err(format!(
                        "plan.nodes[{i}].effect_template.input_bindings source {:?} does not reference item binding {:?}",
                        b.from, binding
                    ));
                }
            }
        }
        if n.kind == PlanNodeKind::IterateUntil {
            require_iterate_hard_bound(n, i)?;
            if n.until
                .as_ref()
                .map(|s| s.trim().is_empty())
                .unwrap_or(true)
                && n.predicates.is_empty()
            {
                return Err(format!(
                    "plan.nodes[{i}].until / predicates required for iterate_until"
                ));
            }
            let _ = validate_row_effect_source_and_template(n, i, &by_id, "iterate_until")?;
        }
    }

    let mut adj: Vec<Vec<usize>> = vec![vec![]; plan.nodes.len()];
    for (i, n) in plan.nodes.iter().enumerate() {
        for d in &n.depends_on {
            let t = *by_id
                .get(d)
                .ok_or_else(|| format!("plan.nodes[{i}].depends_on references unknown id {d:?}"))?;
            adj[i].push(t);
        }
        for u in &n.uses_result {
            let t = *by_id.get(&u.node).ok_or_else(|| {
                format!(
                    "plan.nodes[{i}].uses_result.node {:?} is not a known id",
                    u.node
                )
            })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
        }
        if let Some(source) = &n.source {
            let t = *by_id.get(source).ok_or_else(|| {
                format!("plan.nodes[{i}].source references unknown id {source:?}")
            })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
        }
        if let Some(derive_template) = &n.derive_template {
            if let Some(source) = &derive_template.source {
                let t = *by_id.get(source).ok_or_else(|| {
                    format!(
                        "plan.nodes[{i}].derive_template.source references unknown id {source:?}"
                    )
                })?;
                if !adj[i].contains(&t) {
                    adj[i].push(t);
                }
            }
            for input in &derive_template.inputs {
                let t = *by_id.get(&input.node).ok_or_else(|| {
                    format!(
                        "plan.nodes[{i}].derive_template.inputs references unknown id {:?}",
                        input.node
                    )
                })?;
                if !adj[i].contains(&t) {
                    adj[i].push(t);
                }
            }
        }
        if let Some(compute) = &n.compute {
            let t = *by_id.get(&compute.source).ok_or_else(|| {
                format!(
                    "plan.nodes[{i}].compute.source references unknown id {:?}",
                    compute.source
                )
            })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
            if let ComputeOp::Union { other } = &compute.op {
                let t = *by_id.get(other.as_str()).ok_or_else(|| {
                    format!(
                        "plan.nodes[{i}].compute.union.other references unknown id {:?}",
                        other.as_str()
                    )
                })?;
                if !adj[i].contains(&t) {
                    adj[i].push(t);
                }
            }
        }
        if let Some(relation) = &n.relation {
            let t = *by_id.get(&relation.source).ok_or_else(|| {
                format!(
                    "plan.nodes[{i}].relation.source references unknown id {:?}",
                    relation.source
                )
            })?;
            if !adj[i].contains(&t) {
                adj[i].push(t);
            }
        }
    }
    if has_cycle(&adj) {
        return Err("plan: depends_on has a cycle".to_string());
    }
    let return_value = validate_return_refs(&plan.return_value, &by_id)?;
    let topo = topological_order(plan, &adj)?;
    let mut node_indices = HashMap::new();
    for (id, idx) in &by_id {
        node_indices.insert(PlanNodeId::new(id.clone())?, *idx);
    }
    let nodes = plan
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| validated_node_from_raw(plan, node, i, &by_id))
        .collect::<Result<Vec<_>, _>>()?;
    let approval_gates = plan
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.kind,
                PlanNodeKind::Create
                    | PlanNodeKind::Update
                    | PlanNodeKind::Delete
                    | PlanNodeKind::Action
                    | PlanNodeKind::ForEach
                    | PlanNodeKind::IterateUntil
            ) || matches!(n.effect_class, EffectClass::Write | EffectClass::SideEffect)
        })
        .map(|n| PlanNodeId::new(n.id.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    let artifact = Plan::from_validated_parts(
        plan.version,
        plan.kind,
        plan.name.clone(),
        nodes,
        return_value,
        plan.metadata.clone(),
    );
    let mut validated = ValidatedPlan::from_parts(artifact, topo, node_indices, approval_gates);
    crate::plan_read_bounds::apply_read_budgets(&mut validated);
    Ok(validated)
}

/// Resolve the catalog-qualified row domain of a plan node, walking compute/derive sources.
/// Returns `None` for pure Data literals (no catalog domain). Fail-closed otherwise — no
/// primary-catalog fallback when a catalog-backed source lacks provenance.
pub fn resolve_plan_node_qualified_entity(
    plan: &Plan,
    node_id: &str,
) -> Result<Option<QualifiedEntityKey>, String> {
    let by_id: HashMap<&str, &PlanNode> = plan.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut cur = node_id.to_string();
    for _ in 0..512 {
        let n = by_id
            .get(cur.as_str())
            .ok_or_else(|| format!("uses_result provenance: unknown plan node {cur:?}"))?;
        if n.kind == PlanNodeKind::Data {
            return Ok(None);
        }
        if let Some(qe) = &n.qualified_entity {
            return Ok(Some(qe.clone()));
        }
        if let Some(rel) = &n.relation {
            return Ok(Some(rel.target.clone()));
        }
        if let Some(effect) = &n.effect_template {
            return Ok(Some(effect.qualified_entity.clone()));
        }
        if let Some(compute) = &n.compute {
            cur = compute.source.clone();
            continue;
        }
        if let Some(derive) = &n.derive_template {
            if let Some(src) = &derive.source {
                cur = src.clone();
                continue;
            }
        }
        if let Some(src) = &n.source {
            cur = src.clone();
            continue;
        }
        return Err(format!(
            "uses_result provenance: plan node {cur:?} has no qualified_entity (catalog domain required)"
        ));
    }
    Err(format!(
        "uses_result provenance: exceeded source walk depth from {node_id:?}"
    ))
}

/// Stamp each `uses_result` edge with the source node's [`QualifiedEntityKey`] when the source is
/// catalog-backed. Data literal sources remain unqualified. Rejects contradictory provenance.
pub fn enrich_uses_result_provenance(
    uses: &[PlanResultUse],
    plan: &Plan,
    consumer_id: &str,
) -> Result<Vec<PlanResultUse>, String> {
    let mut out = Vec::with_capacity(uses.len());
    for u in uses {
        let resolved = resolve_plan_node_qualified_entity(plan, u.node.as_str())
            .map_err(|e| format!("plan node {consumer_id:?} uses_result[{:?}]: {e}", u.r#as))?;
        match (u.qualified_entity.as_ref(), resolved) {
            (Some(existing), Some(resolved)) if existing != &resolved => {
                return Err(format!(
                    "plan node {consumer_id:?} uses_result[{:?}] contradictory qualified_entity: edge has {}:{} but source {:?} resolves to {}:{}",
                    u.r#as,
                    existing.entry_id,
                    existing.entity,
                    u.node,
                    resolved.entry_id,
                    resolved.entity
                ));
            }
            (_, Some(resolved)) => {
                out.push(PlanResultUse {
                    node: u.node.clone(),
                    r#as: u.r#as.clone(),
                    qualified_entity: Some(resolved),
                });
            }
            (existing, None) => {
                out.push(PlanResultUse {
                    node: u.node.clone(),
                    r#as: u.r#as.clone(),
                    qualified_entity: existing.cloned(),
                });
            }
        }
    }
    Ok(out)
}

fn validated_node_from_raw(
    plan: &Plan,
    node: &PlanNode,
    node_index: usize,
    by_id: &HashMap<String, usize>,
) -> Result<ValidatedPlanNode, String> {
    let id = PlanNodeId::new(node.id.clone())?;
    let depends_on = typed_node_ids(&node.depends_on)?;
    let uses_result = enrich_uses_result_provenance(&node.uses_result, plan, node.id.as_str())?;
    match node.kind {
        kind @ (PlanNodeKind::Query
        | PlanNodeKind::Search
        | PlanNodeKind::Get
        | PlanNodeKind::Create
        | PlanNodeKind::Update
        | PlanNodeKind::Delete
        | PlanNodeKind::Action) => {
            let ir = node
                .ir
                .as_ref()
                .map(|ir| validated_plan_expr_ir(ir, node_index, "ir"))
                .transpose()?;
            let ir_template = node
                .ir_template
                .as_ref()
                .map(|template| validated_plan_expr_template(template, node_index, "ir_template"))
                .transpose()?;
            Ok(ValidatedPlanNode::Surface(ValidatedSurfaceNode {
                id,
                kind,
                qualified_entity: node.qualified_entity.clone(),
                ir,
                ir_template,
                effect_class: node.effect_class,
                result_shape: node.result_shape,
                projection: node.projection.clone(),
                predicates: node.predicates.clone(),
                depends_on,
                uses_result,
                approval: node.approval.clone(),
                page_size: node.page_size,
                pushed_read_budget: None,
            }))
        }
        PlanNodeKind::Data => Ok(ValidatedPlanNode::Data(ValidatedDataNode {
            id,
            effect_class: node.effect_class,
            result_shape: node.result_shape,
            data: node
                .data
                .clone()
                .ok_or_else(|| format!("plan.nodes[{node_index}].data is required"))?,
            depends_on,
            uses_result,
        })),
        PlanNodeKind::Derive => {
            let template = node
                .derive_template
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{node_index}].derive_template is required"))?;
            let source = template
                .source
                .as_ref()
                .ok_or_else(|| {
                    format!("plan.nodes[{node_index}].derive_template.source is required")
                })
                .and_then(|s| PlanNodeId::new(s.clone()))?;
            let item_binding = template
                .item_binding
                .as_ref()
                .ok_or_else(|| {
                    format!("plan.nodes[{node_index}].derive_template.item_binding is required")
                })
                .and_then(|s| BindingName::new(s.clone()))?;
            let inputs = template
                .inputs
                .iter()
                .map(|input| validated_data_input(plan, input, by_id))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ValidatedPlanNode::Derive(ValidatedDeriveNode {
                id,
                effect_class: node.effect_class,
                result_shape: node.result_shape,
                source,
                item_binding,
                inputs,
                value: template.value.clone(),
                depends_on,
                uses_result,
            }))
        }
        PlanNodeKind::Compute => Ok(ValidatedPlanNode::Compute(ValidatedComputeNode {
            id,
            effect_class: node.effect_class,
            result_shape: node.result_shape,
            compute: node
                .compute
                .clone()
                .ok_or_else(|| format!("plan.nodes[{node_index}].compute is required"))?,
            depends_on,
            uses_result,
        })),
        PlanNodeKind::Relation => {
            let relation = node
                .relation
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{node_index}].relation is required"))?;
            Ok(ValidatedPlanNode::RelationTraversal(
                ValidatedRelationTraversalNode {
                    id,
                    effect_class: node.effect_class,
                    result_shape: node.result_shape,
                    relation: ValidatedPlanRelationTraversal {
                        source: PlanNodeId::new(relation.source.clone())?,
                        relation: RelationName::new(relation.relation.clone())?,
                        target: relation.target.clone(),
                        cardinality: relation.cardinality,
                        source_cardinality: relation.source_cardinality,
                        ir: validated_plan_expr_ir(&relation.ir, node_index, "relation.ir")?,
                        materialize: relation
                            .materialize
                            .clone()
                            .unwrap_or(plasm_core::RelationMaterialization::Unavailable),
                        view_embed_proof: relation.view_embed_proof.clone(),
                        binding_proofs: relation.binding_proofs.clone(),
                    },
                    depends_on,
                    uses_result,
                    pushed_read_budget: None,
                },
            ))
        }
        PlanNodeKind::ForEach => {
            let source = node
                .source
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{node_index}].source is required"))
                .and_then(|s| PlanNodeId::new(s.clone()))?;
            Ok(ValidatedPlanNode::ForEach(ValidatedForEachNode {
                id,
                effect_class: node.effect_class,
                result_shape: node.result_shape,
                source,
                item_binding: node
                    .item_binding
                    .as_ref()
                    .ok_or_else(|| format!("plan.nodes[{node_index}].item_binding is required"))
                    .and_then(|s| BindingName::new(s.clone()))?,
                effect_template: validated_effect_template(
                    node.effect_template.as_ref().ok_or_else(|| {
                        format!("plan.nodes[{node_index}].effect_template is required")
                    })?,
                    node_index,
                )?,
                projection: node.projection.clone(),
                predicates: node.predicates.clone(),
                depends_on,
                uses_result,
                approval: node.approval.clone(),
            }))
        }
        PlanNodeKind::IterateUntil => {
            let source = node
                .source
                .as_ref()
                .ok_or_else(|| format!("plan.nodes[{node_index}].source is required"))
                .and_then(|s| PlanNodeId::new(s.clone()))?;
            let take = require_iterate_hard_bound(node, node_index)?;
            if node.predicates.is_empty() {
                return Err(format!(
                    "plan.nodes[{node_index}].predicates required for iterate_until"
                ));
            }
            Ok(ValidatedPlanNode::IterateUntil(ValidatedIterateUntilNode {
                id,
                effect_class: node.effect_class,
                result_shape: node.result_shape,
                source: source.clone(),
                item_binding: node
                    .item_binding
                    .as_ref()
                    .ok_or_else(|| format!("plan.nodes[{node_index}].item_binding is required"))
                    .and_then(|s| BindingName::new(s.clone()))?,
                effect_template: validated_effect_template(
                    node.effect_template.as_ref().ok_or_else(|| {
                        format!("plan.nodes[{node_index}].effect_template is required")
                    })?,
                    node_index,
                )?,
                until_predicates: node.predicates.clone(),
                take,
                seed_ir: Some({
                    let seed_node = plan
                        .nodes
                        .iter()
                        .find(|n| n.id == source.as_str())
                        .ok_or_else(|| {
                            plasm_core::expr_parser::iterate_seed_must_be_get_identity(
                                source.as_str(),
                            )
                        })?;
                    if seed_node.kind != PlanNodeKind::Get {
                        return Err(plasm_core::expr_parser::iterate_seed_must_be_get_identity(
                            source.as_str(),
                        ));
                    }
                    // Bound identity is `ir_template` at plan time; literal identity is `ir`.
                    // Iterate replays either form (template + binding), not `uses_result.is_empty()`.
                    let seed_replay = if let Some(ir) = seed_node.ir.as_ref() {
                        ir.clone()
                    } else if let Some(template) = seed_node.ir_template.as_ref() {
                        crate::plasm_plan::PlanExprIr {
                            expr: template.expr.clone(),
                            projection: template.projection.clone(),
                            display_expr: template.display_expr.clone(),
                        }
                    } else {
                        return Err(plasm_core::expr_parser::iterate_seed_must_be_get_identity(
                            source.as_str(),
                        ));
                    };
                    validated_plan_expr_ir(&seed_replay, node_index, "iterate_until.seed_ir")?
                }),
                depends_on,
                uses_result,
                approval: node.approval.clone(),
            }))
        }
    }
}

fn typed_node_ids(raw: &[String]) -> Result<Vec<PlanNodeId>, String> {
    raw.iter().cloned().map(PlanNodeId::new).collect()
}

fn validated_data_input(
    plan: &Plan,
    input: &PlanDataInput,
    by_id: &HashMap<String, usize>,
) -> Result<ValidatedPlanDataInput, String> {
    let proof = match input.cardinality {
        InputCardinality::Singleton => InputCardinalityProof::RuntimeCheckedSingleton,
        InputCardinality::Auto => {
            cardinality::analyze_static_cardinality(plan, by_id, input.node.as_str())
                .try_auto_broadcast_input_proof()
                .ok_or_else(|| {
                    format!(
                        "input {:?} is not statically singleton and lacks explicit singleton proof",
                        input.node
                    )
                })?
        }
    };
    Ok(ValidatedPlanDataInput {
        node: PlanNodeId::new(input.node.clone())?,
        alias: InputAlias::new(input.alias.clone())?,
        proof,
    })
}

fn validate_return_refs(
    ret: &PlanReturn,
    by_id: &HashMap<String, usize>,
) -> Result<ValidatedPlanReturn, String> {
    match ret {
        PlanReturn::Node { node } if node.trim().is_empty() => {
            return Err("plan.return node id must be non-empty".to_string());
        }
        PlanReturn::Parallel { nodes } if nodes.is_empty() => {
            return Err("plan.return.nodes must contain at least one node".to_string());
        }
        _ => {}
    }
    for id in return_refs(ret) {
        if !by_id.contains_key(id) {
            return Err(format!("plan.return references unknown id {id:?}"));
        }
    }
    match ret {
        PlanReturn::Node { node } => Ok(ValidatedPlanReturn::Node(PlanNodeId::new(node.clone())?)),
        PlanReturn::Parallel { nodes } => Ok(ValidatedPlanReturn::Parallel {
            parallel: nodes
                .iter()
                .cloned()
                .map(PlanNodeId::new)
                .collect::<Result<Vec<_>, _>>()?,
        }),
    }
}

fn require_iterate_hard_bound(node: &PlanNode, node_index: usize) -> Result<u32, String> {
    let take = node.take.ok_or_else(|| {
        format!("plan.nodes[{node_index}].take is required for iterate_until (hard bound; PLP-8)")
    })?;
    if take == 0 {
        return Err(format!(
            "plan.nodes[{node_index}].take must be ≥ 1 for iterate_until"
        ));
    }
    Ok(take)
}

/// Shared source + item binding + effect-template interpolation checks for `for_each` / `iterate_until`.
fn validate_row_effect_source_and_template<'a>(
    n: &'a PlanNode,
    i: usize,
    by_id: &HashMap<String, usize>,
    kind: &str,
) -> Result<(&'a str, &'a EffectTemplate), String> {
    let source = n
        .source
        .as_ref()
        .ok_or_else(|| format!("plan.nodes[{i}].source is required for {kind}"))?;
    if !by_id.contains_key(source) {
        return Err(format!(
            "plan.nodes[{i}].source references unknown id {source:?}"
        ));
    }
    let binding = n.item_binding.as_deref().unwrap_or_default();
    if binding.trim().is_empty() {
        return Err(format!(
            "plan.nodes[{i}].item_binding is required for {kind}"
        ));
    }
    let template = n
        .effect_template
        .as_ref()
        .ok_or_else(|| format!("plan.nodes[{i}].effect_template is required"))?;
    validate_effect_template(template, i)?;
    let mut input_aliases: Vec<(&str, &str)> = Vec::new();
    for u in &n.uses_result {
        if u.r#as.as_str() != binding {
            input_aliases.push((u.r#as.as_str(), u.node.as_str()));
        }
    }
    let ctx = plasm_core::TemplateRefContext {
        row_binding: Some(binding),
        input_aliases: &input_aliases,
    };
    validate_expression_operands(&template.ir_template.expr, i, &ctx)?;
    Ok((binding, template))
}

fn topological_order(plan: &Plan, adj: &[Vec<usize>]) -> Result<Vec<PlanNodeId>, String> {
    fn visit(
        i: usize,
        plan: &Plan,
        adj: &[Vec<usize>],
        mark: &mut [u8],
        out: &mut Vec<PlanNodeId>,
    ) -> Result<(), String> {
        if mark[i] == 2 {
            return Ok(());
        }
        if mark[i] == 1 {
            return Err("plan: depends_on has a cycle".to_string());
        }
        mark[i] = 1;
        for &d in &adj[i] {
            visit(d, plan, adj, mark, out)?;
        }
        mark[i] = 2;
        out.push(PlanNodeId::new(plan.nodes[i].id.clone())?);
        Ok(())
    }
    let mut mark = vec![0u8; plan.nodes.len()];
    let mut out = Vec::with_capacity(plan.nodes.len());
    for i in 0..plan.nodes.len() {
        visit(i, plan, adj, &mut mark, &mut out)?;
    }
    out.dedup();
    Ok(out)
}

/// Deserialize and validate a program-shaped plan value (same serialized IR as traces/archives).
pub fn validate_plan_value(plan: &serde_json::Value) -> Result<(), String> {
    let plan = parse_plan_value(plan)?;
    validate_plan(&plan)
}
