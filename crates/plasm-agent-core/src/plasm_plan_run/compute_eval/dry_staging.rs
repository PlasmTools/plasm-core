//! Dry-run stub materialization and staged IR template preflight.

use super::super::*;
use super::compute_ops::{render_compute, RenderComputeInput};
use super::eval::{instantiate_expr_template, EvalScope, InputEnv, PlanEvalEnv};
use super::input_rows::{materialized_result_use_inputs, materialized_singleton_inputs};
use std::collections::BTreeMap;

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
        let stub = serde_json::Value::Object(row.clone());
        for label in render_bindings {
            binding_rows.insert(label.as_str().to_string(), vec![stub.clone()]);
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

/// The dry-run [`IoPort`]: every I/O leaf is replaced by typed stub entity rows so downstream
/// `uses_result` resolution can proceed without touching a backend. Returns `None` when there is
/// nothing to stub (entity-optional / page-continuation surfaces, or a foreign-catalog effect
/// target not loaded in this session) — live execute would perform the real effect there.
pub(crate) struct DryIoPort<'a> {
    pub es: &'a ExecuteSession,
}

#[async_trait::async_trait]
impl IoPort for DryIoPort<'_> {
    async fn materialize_io(
        &self,
        step: &IoStep,
        _step_idx: usize,
        materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
    ) -> Result<Option<MaterializedNode>, String> {
        match step {
            IoStep::Surface(surface) => {
                let federated = self.es.contexts_by_entry.len() > 1;
                match crate::plan_surface_policy::surface_qualified_entity_policy_err(
                    surface.id.as_str(),
                    surface,
                    federated,
                )? {
                    crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::PageWithoutEntity
                    | crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::EntityOptional => {
                        Ok(None)
                    }
                    crate::plan_surface_policy::SurfaceQualifiedEntityPolicy::RequiresQualifiedEntity(
                        qe,
                    ) => {
                        let (rows, row_identities) =
                            self.stub_entity_rows(&qe, dry_stub_row_count(surface.result_shape))?;
                        Ok(Some(MaterializedNode::inline_cache(
                            qe.entry_id.to_string(),
                            qe.entity.to_string(),
                            rows,
                            row_identities,
                            surface.display_expr.clone().unwrap_or_default(),
                            Some(surface.projection.clone()).filter(|p| !p.is_empty()),
                        )))
                    }
                }
            }
            IoStep::Relation(relation) => {
                if !materialized.contains_key(&relation.relation.source) {
                    return Err(format!(
                        "dry staging: relation `{}` source `{}` not stubbed",
                        relation.id.as_str(),
                        relation.relation.source.as_str()
                    ));
                }
                let qe = &relation.relation.target;
                let (rows, row_identities) = self.stub_entity_rows(qe, 2)?;
                Ok(Some(MaterializedNode::inline_cache(
                    qe.entry_id.to_string(),
                    qe.entity.to_string(),
                    rows,
                    row_identities,
                    String::new(),
                    relation.relation.ir.projection.clone(),
                )))
            }
            IoStep::ForEach(for_each) => {
                // A `for_each` body invokes a mutator/read per source row. Dry cannot invoke, so it
                // stubs the target entity rows. When that catalog is not loaded here (foreign-catalog
                // policy-gate analysis), there is nothing to stub — live execute fails loudly at the
                // real invoke instead.
                let qe = &for_each.effect_template.qualified_entity;
                let target_loaded = self
                    .es
                    .contexts_by_entry
                    .get(&qe.entry_id)
                    .is_some_and(|ctx| ctx.cgs.entities.contains_key(qe.entity.as_str()));
                if !target_loaded {
                    return Ok(None);
                }
                let (rows, row_identities) =
                    self.stub_entity_rows(qe, dry_stub_row_count(for_each.result_shape))?;
                Ok(Some(MaterializedNode::inline_cache(
                    qe.entry_id.to_string(),
                    qe.entity.to_string(),
                    rows,
                    row_identities,
                    String::new(),
                    Some(for_each.projection.clone()).filter(|p| !p.is_empty()),
                )))
            }
        }
    }
}

impl DryIoPort<'_> {
    /// Resolve a qualified entity in this session and produce `count` typed stub rows for it.
    fn stub_entity_rows(
        &self,
        qe: &QualifiedEntityKey,
        count: usize,
    ) -> Result<(Vec<serde_json::Value>, Vec<Option<plasm_core::RowIdentity>>), String> {
        let scoped = entry_scoped_execute_session(self.es, Some(qe))?;
        let ent = scoped
            .cgs
            .get_entity(qe.entity.as_str())
            .ok_or_else(|| format!("dry staging: unknown entity `{}`", qe.entity))?;
        Ok(dry_stub_entity_rows(ent, count))
    }
}

/// Dry stub materialization of one node, dispatched through the PEC [`ExecStep`] taxonomy. Pure
/// steps run the *shared* pure kernel over inline stub source rows; I/O steps go through
/// [`DryIoPort`]. There is no per-node-kind `match` here — a new node kind is classified once, in
/// [`ExecStep::classify`], and cannot silently bypass dry preflight.
async fn dry_stub_materialize_node(
    es: &ExecuteSession,
    node: &ValidatedPlanNode,
    materialized: &mut BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<(), String> {
    let id = node.id().clone();
    if materialized.contains_key(&id) {
        return Ok(());
    }
    match ExecStep::classify(node.clone()) {
        ExecStep::Pure(pure) => {
            let source = pure.source()?;
            let source_rows = match &source {
                Some(src) => {
                    let source_mat = materialized.get(src).ok_or_else(|| {
                        format!(
                            "dry staging: pure `{}` source `{}` not stubbed",
                            id.as_str(),
                            src.as_str()
                        )
                    })?;
                    source_mat
                        .row_source
                        .inline_rows()
                        .ok_or_else(|| {
                            format!(
                                "dry staging: pure `{}` source `{}` has no inline rows",
                                id.as_str(),
                                src.as_str()
                            )
                        })?
                        .to_vec()
                }
                None => Vec::new(),
            };
            let owner_entry_id = source
                .as_ref()
                .and_then(|src| materialized.get(src).map(|m| m.entry_id.clone()))
                .unwrap_or_else(|| es.entry_id.clone());
            let input_rows = materialized_singleton_inputs(materialized, pure.inputs())?;
            let binding_rows = pure.binding_rows(materialized)?;
            let pm = pure.materialize(
                &PureInputs {
                    source_rows: &source_rows,
                    input_rows: &input_rows,
                    binding_rows: &binding_rows,
                },
                materialized,
            )?;
            materialized.insert(
                id,
                MaterializedNode::inline_cache(
                    owner_entry_id,
                    pm.entity_override.unwrap_or_default(),
                    pm.rows,
                    pm.row_identities,
                    String::new(),
                    None,
                ),
            );
        }
        ExecStep::Io(io) => {
            let port = DryIoPort { es };
            if let Some(stub) = port.materialize_io(&io, 0, materialized).await? {
                materialized.insert(id, stub);
            }
        }
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
        // The dry [`IoPort`] is synchronous by nature (it produces stub rows, never awaits a
        // backend), so the async [`IoPort`] contract shared with live execute resolves in a single
        // poll. `block_on` here is the one justified sync/async bridge: it needs no runtime and
        // cannot pend.
        futures::executor::block_on(async {
            for dep in n.depends_on() {
                let dep_id = dep.clone();
                if !materialized.contains_key(&dep_id) {
                    let dep_node = node_by_id.get(dep.as_str()).ok_or_else(|| {
                        format!(
                            "dry staging: unknown dependency `{dep}` on `{}`",
                            n.id().as_str()
                        )
                    })?;
                    dry_stub_materialize_node(es, dep_node, &mut materialized).await?;
                }
            }
            dry_stub_materialize_node(es, n, &mut materialized).await
        })?;
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
