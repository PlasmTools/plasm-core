//! Dry-run stub materialization and staged IR template preflight.

use super::super::*;
use super::eval::{instantiate_expr_template, EvalScope, InputEnv, PlanEvalEnv};
use super::input_rows::{materialized_result_use_inputs, materialized_singleton_inputs};
use std::collections::BTreeMap;

fn dry_stub_row_count(shape: crate::plasm_plan::ResultShape) -> usize {
    use crate::plasm_plan::ResultShape;
    match shape {
        ResultShape::List | ResultShape::Page => 2,
        _ => 1,
    }
}

fn dry_stub_entity_rows(
    cgs: &plasm_core::CGS,
    ent: &plasm_core::EntityDef,
    count: usize,
) -> Result<(Vec<serde_json::Value>, Vec<Option<plasm_core::RowIdentity>>), String> {
    let rows = plasm_core::dry_stub_entity_row_json(cgs, ent, count)?;
    Ok((rows, vec![None; count]))
}

/// Dry validation replaces backend leaves with typed stub entity rows so downstream
/// `uses_result` resolution can proceed without touching a backend. Returns `None` when there is
/// nothing to stub (entity-optional / page-continuation surfaces, or a foreign-catalog effect
/// target not loaded in this session) — live execute would perform the real effect there.
async fn dry_stub_materialize_io(
    es: &ExecuteSession,
    step: &IoStep,
    materialized: &BTreeMap<PlanNodeId, MaterializedNode>,
) -> Result<Option<MaterializedNode>, String> {
    match step {
        IoStep::Surface(surface) => {
            let federated = es.contexts_by_entry.len() > 1;
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
                            dry_stub_entity_rows_for(es, &qe, dry_stub_row_count(surface.result_shape))?;
                        Ok(Some(MaterializedNode::inline_cache(
                            qe.clone(),
                            rows,
                            row_identities,
                            crate::plasm_plan_run::dry::render_surface_operation(surface),
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
            let (rows, row_identities) = dry_stub_entity_rows_for(es, qe, 2)?;
            Ok(Some(MaterializedNode::inline_cache(
                qe.clone(),
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
            let target_loaded = es
                .contexts_by_entry
                .get(&qe.entry_id)
                .is_some_and(|ctx| ctx.cgs.entities.contains_key(qe.entity.as_str()));
            if !target_loaded {
                return Ok(None);
            }
            let (rows, row_identities) =
                dry_stub_entity_rows_for(es, qe, dry_stub_row_count(for_each.result_shape))?;
            Ok(Some(MaterializedNode::inline_cache(
                qe.clone(),
                rows,
                row_identities,
                String::new(),
                Some(for_each.projection.clone()).filter(|p| !p.is_empty()),
            )))
        }
        IoStep::IterateUntil(it) => {
            // Dry: stub final singleton state for the seed entity (step effects not invoked).
            let qe = &it.effect_template.qualified_entity;
            let target_loaded = es
                .contexts_by_entry
                .get(&qe.entry_id)
                .is_some_and(|ctx| ctx.cgs.entities.contains_key(qe.entity.as_str()));
            if !target_loaded {
                return Ok(None);
            }
            let (rows, row_identities) =
                dry_stub_entity_rows_for(es, qe, dry_stub_row_count(it.result_shape))?;
            Ok(Some(MaterializedNode::inline_cache(
                qe.clone(),
                rows,
                row_identities,
                String::new(),
                None,
            )))
        }
    }
}

/// Resolve a qualified entity in this session and produce typed dry-validation rows.
fn dry_stub_entity_rows_for(
    es: &ExecuteSession,
    qe: &QualifiedEntityKey,
    count: usize,
) -> Result<(Vec<serde_json::Value>, Vec<Option<plasm_core::RowIdentity>>), String> {
    let scoped = entry_scoped_execute_session(es, Some(qe))?;
    let ent = scoped
        .cgs
        .get_entity(qe.entity.as_str())
        .ok_or_else(|| format!("dry staging: unknown entity `{}`", qe.entity))?;
    let (rows, _) = dry_stub_entity_rows(scoped.cgs.as_ref(), ent, count)?;
    let keys: Vec<String> = ent.key_vars.iter().map(ToString::to_string).collect();
    let identities = rows
        .iter()
        .map(|row| {
            let scalar = |key: &str| -> Result<String, String> {
                let value = row
                    .get(key)
                    .ok_or_else(|| format!("dry identity lacks `{key}`"))?;
                plasm_core::operand_binding::IdentityCodec::compile(
                    scoped.cgs.as_ref(),
                    plasm_core::operand_binding::IdentityTarget {
                        entity: &ent.name,
                        field: (keys.len() > 1).then_some(key),
                    },
                )?
                .encode(value)
                .map(|id| id.to_string())
            };
            let reference = if keys.len() > 1 {
                let parts = keys
                    .iter()
                    .map(|key| scalar(key).map(|value| (key.clone(), value)))
                    .collect::<Result<std::collections::BTreeMap<_, _>, _>>()?;
                plasm_core::Ref::compound(ent.name.to_string(), parts)
            } else {
                plasm_core::Ref::new(
                    ent.name.to_string(),
                    scalar(
                        keys.first()
                            .map(String::as_str)
                            .unwrap_or(ent.id_field.as_str()),
                    )?,
                )
            };
            Ok(Some(plasm_core::row_composition::row_identity_from_parts(
                plasm_core::QualifiedEntityKey {
                    entry_id: qe.entry_id.clone().into(),
                    entity: qe.entity.clone().into(),
                },
                reference,
                &indexmap::IndexMap::new(),
                ent.id_field.as_str(),
                &keys,
            )))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok((rows, identities))
}

/// Dry stub materialization of one node, dispatched through the PEC [`ExecStep`] taxonomy. Pure
/// steps run the shared pure kernel over inline stub source rows. There is no adapter-facing
/// plan-node API: a new node kind is classified once, in
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
                .and_then(|src| {
                    materialized
                        .get(src)
                        .map(|m| m.qualified_entity.entry_id.clone())
                })
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
                    QualifiedEntityKey {
                        entry_id: owner_entry_id,
                        entity: pm.entity_override.unwrap_or_default(),
                    },
                    pm.rows,
                    pm.row_identities,
                    String::new(),
                    None,
                ),
            );
        }
        ExecStep::Io(io) => {
            if let Some(stub) = dry_stub_materialize_io(es, &io, materialized).await? {
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
    let mut synthetic = std::collections::BTreeSet::new();
    let mut deferred = std::collections::BTreeSet::new();
    for n in &plan.nodes {
        // Backend stubs witness types, never values. Every pure computation
        // over unknown observations (render, derive, arithmetic, filtering,
        // aggregation, etc.) is deferred along with its dependents. Evaluating
        // even one such operator on invented values can reject a lawful plan.
        let has_synthetic_input = n.depends_on().iter().any(|id| synthetic.contains(id));
        let step = ExecStep::classify(n.clone());
        if matches!(step, ExecStep::Io(_)) || has_synthetic_input {
            synthetic.insert(n.id().clone());
        }
        let needs_real_values = has_synthetic_input && matches!(step, ExecStep::Pure(_));
        if needs_real_values || n.depends_on().iter().any(|id| deferred.contains(id)) {
            deferred.insert(n.id().clone());
            continue;
        }
        // Dry validation performs no backend exchange. `block_on` is only the sync bridge for the
        // shared async materialization helpers used by the closed execution machine.
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
        let empty_coercion = BTreeMap::new();
        let env = PlanEvalEnv {
            scope,
            inputs,
            wire_coercion_by_alias: &empty_coercion,
        };
        let scoped_es = entry_scoped_execute_session(es, surface.qualified_entity.as_ref())?;
        instantiate_expr_template(template, &env, &scoped_es.cgs)?;
    }
    Ok(())
}

#[cfg(test)]
mod dry_stub_tests {
    use super::dry_stub_entity_rows;
    use plasm_core::load_schema;
    use std::path::PathBuf;

    #[test]
    fn dry_stub_lang_item_score_is_integer_json() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let cgs = load_schema(&root.join("../../fixtures/schemas/plasm_language_matrix"))
            .expect("load matrix");
        let ent = cgs.get_entity("LangItem").expect("LangItem");
        let (rows, _) = dry_stub_entity_rows(&cgs, ent, 2).expect("stubs");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["score"], serde_json::json!(0));
        assert_eq!(rows[1]["score"], serde_json::json!(1));
        assert!(rows[0]["score"].is_i64() || rows[0]["score"].is_u64());
        assert!(
            rows[0]["active"].is_boolean(),
            "active={}",
            rows[0]["active"]
        );
    }
}
