//! Scoped query fanout and parent-get prefer partitioning.

use super::*;

pub(crate) fn partition_scoped_query_fanout<F>(
    parents: &[CachedEntity],
    mut build_scoped_query: F,
) -> Vec<(usize, QueryExpr)>
where
    F: FnMut(&CachedEntity) -> QueryExpr,
{
    parents
        .iter()
        .enumerate()
        .map(|(i, parent)| (i, build_scoped_query(parent)))
        .collect()
}

fn fallback_query_capability<'a>(
    fallback: &RelationScopedFallback,
    cgs: &'a CGS,
) -> Option<&'a CapabilitySchema> {
    match fallback {
        RelationScopedFallback::QueryScoped { capability, .. }
        | RelationScopedFallback::QueryScopedBindings { capability, .. } => {
            cgs.get_capability(capability.as_str())
        }
        RelationScopedFallback::HydrateFromEmbedPath { .. } => None,
    }
}

pub(crate) fn build_scoped_query_from_fallback(
    fallback: &RelationScopedFallback,
    parent: &CachedEntity,
    parent_entity_def: &EntityDef,
    target_entity: &EntityName,
    cgs: &CGS,
) -> Result<QueryExpr, RuntimeError> {
    match fallback {
        RelationScopedFallback::QueryScoped { capability, param } => {
            let id_field = cgs
                .get_entity(parent.reference.entity_type.as_str())
                .map(|def| def.id_field.as_str().to_string())
                .unwrap_or_default();
            let id = parent
                .get_field(id_field.as_str())
                .map(|tf| tf.to_value())
                .and_then(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Integer(n) => Some(n.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| parent.reference.primary_slot_str());
            let pred = Predicate::eq(param.as_str(), id);
            let mut q = QueryExpr::filtered(target_entity.clone(), pred);
            q.capability_name = Some(capability.clone());
            Ok(q)
        }
        RelationScopedFallback::QueryScopedBindings {
            capability,
            bindings,
        } => {
            let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
                RuntimeError::ConfigurationError {
                    message: format!("unknown fallback capability '{capability}'"),
                }
            })?;
            let cap_params: Vec<_> = cap.query_surface_fields().cloned().collect();
            let preds: Vec<Predicate> = bindings
                .iter()
                .map(|(cap_param, parent_field)| {
                    let raw = chain_binding_raw_json(parent, parent_entity_def, parent_field);
                    let value =
                        chain_binding_plasm_value(&raw, cap_param.as_str(), &cap_params, cgs);
                    Predicate::eq(cap_param.as_str(), value)
                })
                .collect();
            let pred = if preds.len() == 1 {
                preds.into_iter().next().expect("non-empty preds")
            } else {
                Predicate::and(preds)
            };
            let mut q = QueryExpr::filtered(target_entity.clone(), pred);
            q.capability_name = Some(capability.clone());
            Ok(q)
        }
        RelationScopedFallback::HydrateFromEmbedPath { .. } => {
            Err(RuntimeError::ConfigurationError {
                message: "hydrate_from_embed_path fallback is plan-materialized only".into(),
            })
        }
    }
}

/// Partition for [`RelationMaterialization::PreferFromParentGet`] using [`resolve_relation_row_resolution`].
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn partition_prefer_from_parent_get(
    parents: &[CachedEntity],
    materialize: &RelationMaterialization,
    relation_key: &str,
    expected_target: &str,
    mat: &SessionMaterialization,
    parent_entity_def: &EntityDef,
    cgs: &CGS,
    target_entity: &EntityName,
    fallback: &RelationScopedFallback,
) -> Result<(Vec<Vec<CachedEntity>>, Vec<(usize, QueryExpr)>), RuntimeError> {
    let n = parents.len();
    let parent_rows: Vec<(serde_json::Value, Option<Vec<Ref>>)> = parents
        .iter()
        .map(|parent| {
            (
                parent.payload_to_json(),
                parent.relations.get(relation_key).map(|refs| refs.to_vec()),
            )
        })
        .collect();
    let parent_row_refs: Vec<(&serde_json::Value, Option<&[Ref]>)> = parent_rows
        .iter()
        .map(|(json, refs)| (json, refs.as_deref()))
        .collect();
    let resolutions = partition_prefer_resolutions(
        materialize,
        relation_key,
        expected_target,
        parent_row_refs,
        |r| mat.get(r).is_some(),
    );
    let mut per_parent: Vec<Vec<CachedEntity>> = (0..n).map(|_| Vec::new()).collect();
    let mut network_jobs: Vec<(usize, QueryExpr)> = Vec::new();
    for (i, resolution) in resolutions.into_iter().enumerate() {
        let parent = &parents[i];
        match resolution {
            RelationRowResolution::EmbeddedRefs(refs) => {
                per_parent[i] =
                    resolve_cached_targets_from_relation_refs(mat, &refs, expected_target)?;
            }
            RelationRowResolution::ScopedQuery => {
                let mut q = build_scoped_query_from_fallback(
                    fallback,
                    parent,
                    parent_entity_def,
                    target_entity,
                    cgs,
                )?;
                if let Some(child_cap) = fallback_query_capability(fallback, cgs) {
                    relation_inherit_for_scoped_query(cgs, mat, parent, child_cap, &q)?
                        .apply_to_scoped_query(&mut q);
                }
                network_jobs.push((i, q));
            }
        }
    }
    Ok((per_parent, network_jobs))
}

pub(crate) fn resolve_cached_targets_from_relation_refs(
    mat: &SessionMaterialization,
    refs: &[Ref],
    expected_target: &str,
) -> Result<Vec<CachedEntity>, RuntimeError> {
    let mut out = Vec::with_capacity(refs.len());
    for r in refs {
        if r.entity_type.as_str() != expected_target {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "Decoded relation expected Ref.entity_type {expected_target}, got {}",
                    r.entity_type
                ),
            });
        }
        let Some(e) = mat.get(r) else {
            return Err(RuntimeError::CacheError {
                message: format!("missing embedded relation target in session graph: {r}"),
            });
        };
        out.push(e.clone());
    }
    Ok(out)
}

pub(crate) fn ref_from_materialize_bindings_for_get_chain(
    target_ent: &EntityDef,
    binding_values: &IndexMap<String, String>,
) -> Result<Ref, RuntimeError> {
    if !target_ent.key_vars.is_empty() {
        let mut parts = BTreeMap::new();
        for kv in &target_ent.key_vars {
            let s = binding_values.get(kv.as_str()).ok_or_else(|| {
                RuntimeError::ConfigurationError {
                    message: format!(
                        "get_scoped_bindings missing bound value for `{}` on entity `{}`",
                        kv, target_ent.name
                    ),
                }
            })?;
            parts.insert(kv.to_string(), s.clone());
        }
        Ok(Ref::compound(target_ent.name.clone(), parts))
    } else {
        let id = binding_values
            .get(target_ent.id_field.as_str())
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!(
                    "get_scoped_bindings missing bound value for id field `{}` on entity `{}`",
                    target_ent.id_field, target_ent.name
                ),
            })?;
        Ok(Ref::new(target_ent.name.clone(), id.clone()))
    }
}

/// Raw JSON for a `query_scoped_bindings` parent field from a cached row / ref.
pub(crate) fn chain_binding_raw_json(
    entity: &CachedEntity,
    parent_def: &plasm_core::EntityDef,
    parent_field: &EntityFieldName,
) -> serde_json::Value {
    let pf = parent_field.as_str();
    if let Some(v) = entity.get_field(pf) {
        return plasm_core::plasm_value_to_json(&v.to_value());
    }
    if pf == parent_def.id_field.as_str() {
        return serde_json::Value::String(entity.reference.primary_slot_str());
    }
    if let EntityKey::Compound(parts) = &entity.reference.key {
        if let Some(s) = parts.get(pf).and_then(|slot| slot.as_lit_str()) {
            return serde_json::Value::String(s.to_string());
        }
    }
    serde_json::Value::String(entity.reference.primary_slot_str())
}

pub(crate) fn chain_binding_plasm_value(
    raw: &serde_json::Value,
    cap_param: &str,
    cap_params: &[plasm_core::InputFieldSchema],
    cgs: &CGS,
) -> Value {
    let Some(param_schema) = cap_params.iter().find(|f| f.name == cap_param) else {
        return plasm_core::json_value_to_plasm_value(raw);
    };
    let Ok(nv) = param_schema.named_value(cgs) else {
        return plasm_core::json_value_to_plasm_value(raw);
    };
    plasm_core::binding_value_as_plasm_value(raw, nv)
}

/// String slot for get_scoped_bindings path keys (compound ref parts).
pub(crate) fn chain_binding_value(
    entity: &CachedEntity,
    parent_def: &plasm_core::EntityDef,
    parent_field: &EntityFieldName,
) -> String {
    let v = chain_binding_raw_json(entity, parent_def, parent_field);
    match plasm_core::json_value_to_plasm_value(&v) {
        Value::String(s) if !s.is_empty() => s,
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => entity.reference.primary_slot_str(),
    }
}
