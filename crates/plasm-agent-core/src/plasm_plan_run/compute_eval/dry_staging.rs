//! Dry-run stub materialization and staged IR template preflight.

use super::super::*;
use super::compute_ops::{render_compute, RenderComputeInput};
use super::eval::{instantiate_expr_template, EvalScope, InputEnv, PlanEvalEnv};
use super::input_rows::materialized_result_use_inputs;
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) fn dry_validate_render_nodes(
    es: &ExecuteSession,
    plan: &crate::plasm_plan::Plan<crate::plasm_plan::ValidatedPlanState>,
) -> Result<(), String> {
    use crate::plasm_plan::{ComputeOp, ValidatedPlanNode};
    use std::collections::HashMap;

    let nodes: HashMap<String, &ValidatedPlanNode> = plan
        .nodes
        .iter()
        .map(|n| (n.id().as_str().to_string(), n))
        .collect();
    for n in &plan.nodes {
        let ValidatedPlanNode::Compute(c) = n else {
            continue;
        };
        let ComputeOp::Render {
            columns,
            template,
            column_aliases,
            render_bindings,
        } = &c.compute.op
        else {
            continue;
        };
        let qe = dry_render_source_qualified_entity(&nodes, c.compute.source.clone())?;
        let scoped = entry_scoped_execute_session(es, Some(&qe))?;
        let ent = scoped
            .cgs
            .get_entity(qe.entity.as_str())
            .ok_or_else(|| format!("dry render: unknown entity `{}`", qe.entity))?;
        let mut row = serde_json::Map::new();
        for field in ent.fields.keys() {
            row.insert(field.as_str().to_string(), serde_json::Value::Null);
        }
        row.insert(
            ent.id_field.as_str().to_string(),
            serde_json::Value::String("dry-placeholder".into()),
        );
        let mut binding_rows = BTreeMap::new();
        for label in render_bindings {
            binding_rows.insert(label.as_str().to_string(), vec![serde_json::Value::Null]);
        }
        if let Some(alias) = c.compute.collection_alias.as_ref() {
            if !binding_rows.contains_key(alias.as_str()) {
                binding_rows.insert(alias.as_str().to_string(), vec![serde_json::Value::Null]);
            }
        }
        render_compute(&RenderComputeInput {
            primary_rows: &[serde_json::Value::Object(row)],
            columns: &RenderColumns::from_op_parts(columns.clone(), column_aliases.clone()),
            template,
            collection_alias: c.compute.collection_alias.as_ref(),
            render_bindings,
            binding_rows: &binding_rows,
        })?;
    }
    Ok(())
}

fn dry_render_source_qualified_entity(
    nodes: &std::collections::HashMap<String, &ValidatedPlanNode>,
    mut source: String,
) -> Result<QualifiedEntityKey, String> {
    use crate::plasm_plan::ValidatedPlanNode;

    loop {
        let Some(n) = nodes.get(source.as_str()) else {
            return Err(format!("dry render: unknown source node `{source}`"));
        };
        match n {
            ValidatedPlanNode::Surface(s) => {
                match crate::plan_surface_policy::surface_qualified_entity_policy(s, false) {
                    Ok(crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::PageWithoutEntity) => {
                        return Err(format!(
                            "dry render: surface `{source}` has no qualified entity (page continuation cannot be a render source)"
                        ));
                    }
                    Ok(
                        crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::EntityOptional,
                    ) => {
                        return Err(format!(
                            "dry render: surface `{source}` has no qualified entity"
                        ));
                    }
                    Ok(
                        crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::RequiresQualifiedEntity(
                            qe,
                        ),
                    ) => return Ok(qe),
                    Err(reason) => {
                        return Err(format!(
                            "dry render: surface `{source}` has no qualified entity: {reason}"
                        ));
                    }
                }
            }
            ValidatedPlanNode::RelationTraversal(r) => return Ok(r.relation.target.clone()),
            ValidatedPlanNode::Compute(c) => {
                source = c.compute.source.clone();
            }
            other => {
                return Err(format!(
                    "dry render: source `{source}` is {:?}, expected surface/relation/compute chain",
                    other.kind()
                ));
            }
        }
    }
}

fn dry_stub_row_count(shape: crate::plasm_plan::ResultShape) -> usize {
    use crate::plasm_plan::ResultShape;
    match shape {
        ResultShape::List | ResultShape::Page => 2,
        _ => 1,
    }
}

fn dry_stub_entity_rows(
    ent: &plasm_core::EntityDef,
    count: usize,
) -> (Vec<serde_json::Value>, Vec<Option<plasm_core::RowIdentity>>) {
    let mut rows = Vec::with_capacity(count);
    for i in 0..count {
        let mut obj = serde_json::Map::new();
        for field in ent.fields.keys() {
            obj.insert(
                field.as_str().to_string(),
                serde_json::Value::String(format!("dry-{i}")),
            );
        }
        obj.insert(
            ent.id_field.as_str().to_string(),
            serde_json::Value::String(format!("dry-{i}")),
        );
        rows.push(serde_json::Value::Object(obj));
    }
    (rows, vec![None; count])
}

fn dry_stub_materialize_node(
    es: &ExecuteSession,
    node: &ValidatedPlanNode,
    materialized: &mut BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<(), String> {
    use crate::plasm_plan::ValidatedPlanNode;
    use plasm_runtime::{ExecutionResult, ExecutionSource, ExecutionStats};

    let id = node.id().clone();
    if materialized.contains_key(&id) {
        return Ok(());
    }
    match node {
        ValidatedPlanNode::Surface(surface) => {
            let federated = es.contexts_by_entry.len() > 1;
            match crate::plan_surface_policy::surface_qualified_entity_policy_err(
                id.as_str(),
                surface,
                federated,
            )? {
                crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::PageWithoutEntity
                | crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::EntityOptional => {
                    return Ok(());
                }
                crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::RequiresQualifiedEntity(
                    qe,
                ) => {
                    let scoped = entry_scoped_execute_session(es, Some(&qe))?;
                    let ent = scoped
                        .cgs
                        .get_entity(qe.entity.as_str())
                        .ok_or_else(|| format!("dry staging: unknown entity `{}`", qe.entity))?;
                    let count = dry_stub_row_count(surface.result_shape);
                    let (rows, row_identities) = dry_stub_entity_rows(ent, count);
                    materialized.insert(
                        id,
                        MaterializedNode {
                            entry_id: qe.entry_id.to_string(),
                            entity: qe.entity.to_string(),
                            result: Arc::new(ExecutionResult {
                                count: rows.len(),
                                entities: Vec::new(),
                                has_more: false,
                                pagination_resume: None,
                                paging_handle: None,
                                source: ExecutionSource::Cache,
                                stats: ExecutionStats::default(),
                                request_fingerprints: vec![],
                            }),
                            row_source: inline_row_source_owned(rows),
                            row_identities,
                            artifact: None,
                            display: surface.display_expr.clone().unwrap_or_default(),
                            projection: Some(surface.projection.clone()).filter(|p| !p.is_empty()),
                        },
                    );
                }
            }
        }
        ValidatedPlanNode::Compute(compute) => {
            let source_id = PlanNodeId::new(compute.compute.source.clone())?;
            if !materialized.contains_key(&source_id) {
                return Err(format!(
                    "dry staging: compute `{}` source `{}` not stubbed",
                    id.as_str(),
                    source_id.as_str()
                ));
            }
            let fields: Vec<String> = compute
                .compute
                .schema
                .fields
                .iter()
                .map(|f| f.name.as_str().to_string())
                .collect();
            let mut obj = serde_json::Map::new();
            for field in &fields {
                obj.insert(field.clone(), serde_json::Value::String("dry".into()));
            }
            let row = serde_json::Value::Object(obj);
            materialized.insert(
                id,
                MaterializedNode {
                    entry_id: String::new(),
                    entity: String::new(),
                    result: Arc::new(ExecutionResult {
                        count: 1,
                        entities: Vec::new(),
                        has_more: false,
                        pagination_resume: None,
                        paging_handle: None,
                        source: ExecutionSource::Cache,
                        stats: ExecutionStats::default(),
                        request_fingerprints: vec![],
                    }),
                    row_source: inline_row_source_owned(vec![row]),
                    row_identities: vec![None],
                    artifact: None,
                    display: String::new(),
                    projection: None,
                },
            );
        }
        ValidatedPlanNode::RelationTraversal(relation) => {
            let source_id = relation.relation.source.clone();
            if !materialized.contains_key(&source_id) {
                return Err(format!(
                    "dry staging: relation `{}` source `{}` not stubbed",
                    id.as_str(),
                    source_id.as_str()
                ));
            }
            let qe = &relation.relation.target;
            let scoped = entry_scoped_execute_session(es, Some(qe))?;
            let ent = scoped
                .cgs
                .get_entity(qe.entity.as_str())
                .ok_or_else(|| format!("dry staging: unknown entity `{}`", qe.entity))?;
            let (rows, row_identities) = dry_stub_entity_rows(ent, 2);
            materialized.insert(
                id,
                MaterializedNode {
                    entry_id: qe.entry_id.to_string(),
                    entity: qe.entity.to_string(),
                    result: Arc::new(ExecutionResult {
                        count: rows.len(),
                        entities: Vec::new(),
                        has_more: false,
                        pagination_resume: None,
                        paging_handle: None,
                        source: ExecutionSource::Cache,
                        stats: ExecutionStats::default(),
                        request_fingerprints: vec![],
                    }),
                    row_source: inline_row_source_owned(rows),
                    row_identities,
                    artifact: None,
                    display: String::new(),
                    projection: relation.relation.ir.projection.clone(),
                },
            );
        }
        ValidatedPlanNode::Data(_)
        | ValidatedPlanNode::Derive(_)
        | ValidatedPlanNode::ForEach(_) => {}
    }
    Ok(())
}

/// Preflight staged IR template instantiation (same singleton/column rules as live execute).
pub(crate) fn dry_validate_staged_surfaces(
    es: &ExecuteSession,
    plan: &crate::plasm_plan::Plan<crate::plasm_plan::ValidatedPlanState>,
) -> Result<(), String> {
    use crate::plasm_plan::ValidatedPlanNode;

    let node_by_id: std::collections::HashMap<String, &ValidatedPlanNode> = plan
        .nodes
        .iter()
        .map(|n| (n.id().as_str().to_string(), n))
        .collect();
    let mut materialized: BTreeMap<PlanNodeId, MaterializedNode> = BTreeMap::new();
    for n in &plan.nodes {
        for dep in n.depends_on() {
            let dep_id = dep.clone();
            if !materialized.contains_key(&dep_id) {
                let dep_node = node_by_id.get(dep.as_str()).ok_or_else(|| {
                    format!(
                        "dry staging: unknown dependency `{dep}` on `{}`",
                        n.id().as_str()
                    )
                })?;
                dry_stub_materialize_node(es, dep_node, &mut materialized)?;
            }
        }
        dry_stub_materialize_node(es, n, &mut materialized)?;
        let ValidatedPlanNode::Surface(surface) = n else {
            continue;
        };
        let Some(template) = surface.ir_template.as_ref() else {
            continue;
        };
        if surface.uses_result.is_empty() {
            continue;
        }
        let input_rows =
            materialized_result_use_inputs(&materialized, &surface.uses_result, Some(template))?;
        let scope = EvalScope::Root {
            row: &serde_json::Value::Null,
        };
        let inputs = InputEnv { rows: &input_rows };
        let env = PlanEvalEnv {
            scope,
            inputs,
            wire_coercion: None,
        };
        instantiate_expr_template(template, &env)?;
    }
    Ok(())
}
