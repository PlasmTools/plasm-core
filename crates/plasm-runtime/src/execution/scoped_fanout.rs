//! Scoped query fanout and parent-get prefer partitioning.

use super::*;

#[cfg(test)]
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
                RuntimeError::CapabilityUnknown {
                    capability: capability.to_string(),
                }
            })?;
            let cap_params: Vec<_> = cap.query_surface_fields().cloned().collect();
            let preds: Vec<Predicate> = bindings
                .iter()
                .map(|(cap_param, parent_field)| {
                    let raw = chain_binding_raw_json(parent, parent_entity_def, parent_field, cgs)?;
                    let value =
                        chain_binding_plasm_value(&raw, cap_param.as_str(), &cap_params, cgs)?;
                    Ok(Predicate::eq(cap_param.as_str(), value))
                })
                .collect::<Result<_, RuntimeError>>()?;
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
            Err(RuntimeError::EmbeddedHydrationPlanRequired)
        }
    }
}

/// Partition for [`RelationMaterialization::PreferFromParentGet`] using [`resolve_relation_row_resolution`].
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn partition_prefer_from_parent_get(
    parents: &plasm_core::collection_codec::SharedRows<CachedEntity>,
    materialize: &RelationMaterialization,
    relation_key: &str,
    expected_target: &str,
    mat: &SessionMaterialization,
    parent_entity_def: &EntityDef,
    cgs: &CGS,
    target_entity: &EntityName,
    fallback: &RelationScopedFallback,
) -> Result<(Vec<Option<ExecutionCollection>>, Vec<(usize, QueryExpr)>), RuntimeError> {
    let n = parents.len();
    let parent_rows: Vec<(plasm_core::ValueRow, Option<Vec<Ref>>)> = parents
        .iter()
        .map(|parent| {
            Ok((
                parent.to_row_values(Some(cgs))?,
                parent
                    .relations
                    .get(relation_key)
                    .map(|refs| refs.iter().cloned().collect()),
            ))
        })
        .collect::<Result<_, plasm_core::ValueCodecError>>()?;
    let parent_row_refs: Vec<(&plasm_core::Value, Option<&[Ref]>)> = parent_rows
        .iter()
        .map(|(row, refs)| (&**row, refs.as_deref()))
        .collect();
    let resolutions = partition_prefer_resolutions(
        materialize,
        parent_entity_def
            .relations
            .get(relation_key)
            .expect("validated relation definition")
            .cardinality,
        relation_key,
        expected_target,
        parent_row_refs,
        |r| mat.get(r).is_some(),
    );
    let mut per_parent = vec![None; n];
    let mut network_jobs: Vec<(usize, QueryExpr)> = Vec::new();
    for (i, resolution) in resolutions.into_iter().enumerate() {
        let parent = &parents[i];
        match resolution {
            RelationRowResolution::EmbeddedRefs(refs) => {
                let rows = resolve_cached_targets_from_relation_refs(mat, &refs, expected_target)?;
                let membership = parent.relations.get(relation_key).ok_or_else(|| {
                    crate::CacheError::RelationUnobserved {
                        reference: parent.reference.clone(),
                        relation: relation_key.to_owned(),
                    }
                })?;
                per_parent[i] = Some(ExecutionCollection::materialized(
                    membership.record().clone(),
                    rows,
                )?);
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

pub(crate) fn resolve_cached_targets_from_relation_refs<'a>(
    mat: &SessionMaterialization,
    refs: impl IntoIterator<Item = &'a Ref>,
    expected_target: &str,
) -> Result<plasm_core::collection_codec::SharedRows<CachedEntity>, RuntimeError> {
    let mut out = Vec::new();
    for r in refs {
        if r.entity_type.as_str() != expected_target {
            return Err(RuntimeError::TraversalParentTypeMismatch {
                expected: expected_target.to_owned(),
                actual: r.entity_type.to_string(),
            });
        }
        let Some(e) = mat.shared_row(r) else {
            return Err(crate::CacheError::EntityMissing {
                reference: r.clone(),
            }
            .into());
        };
        out.push(e);
    }
    Ok(plasm_core::collection_codec::SharedRows::concat(&out))
}

pub(crate) fn ref_from_materialize_bindings_for_get_chain(
    target_ent: &EntityDef,
    binding_values: &IndexMap<String, String>,
) -> Result<Ref, RuntimeError> {
    if !target_ent.key_vars.is_empty() {
        let mut parts = BTreeMap::new();
        for kv in &target_ent.key_vars {
            let s = binding_values.get(kv.as_str()).ok_or_else(|| {
                RuntimeError::MaterializeBindingMissing {
                    entity: target_ent.name.to_string(),
                    field: kv.to_string(),
                }
            })?;
            parts.insert(kv.to_string(), s.clone());
        }
        Ok(Ref::compound(target_ent.name.clone(), parts))
    } else {
        let id = binding_values
            .get(target_ent.id_field.as_str())
            .ok_or_else(|| RuntimeError::MaterializeBindingMissing {
                entity: target_ent.name.to_string(),
                field: target_ent.id_field.to_string(),
            })?;
        Ok(Ref::new(target_ent.name.clone(), id.clone()))
    }
}

/// Raw JSON for a `query_scoped_bindings` parent field from a cached row / ref.
pub(crate) fn chain_binding_raw_json(
    entity: &CachedEntity,
    parent_def: &plasm_core::EntityDef,
    parent_field: &EntityFieldName,
    cgs: &CGS,
) -> Result<serde_json::Value, RuntimeError> {
    let field = parent_field.as_str();
    if let Some(value) = entity.get_field(field) {
        return Ok(plasm_core::try_plasm_value_to_json(&value.to_value())?);
    }
    let slot = if field == parent_def.id_field.as_str() {
        Some(entity.reference.primary_slot_str())
    } else if let EntityKey::Compound(parts) = &entity.reference.key {
        parts
            .get(field)
            .and_then(|slot| slot.as_lit_str())
            .map(str::to_owned)
    } else {
        None
    };
    let slot = slot.ok_or_else(|| RuntimeError::MaterializeBindingMissing {
        entity: parent_def.name.to_string(),
        field: field.into(),
    })?;
    Ok(plasm_core::identity_slot_to_json(
        cgs, parent_def, field, &slot,
    )?)
}

pub(crate) fn chain_binding_plasm_value(
    raw: &serde_json::Value,
    cap_param: &str,
    cap_params: &[plasm_core::InputFieldSchema],
    cgs: &CGS,
) -> Result<Value, RuntimeError> {
    let schema = cap_params
        .iter()
        .find(|field| field.name == cap_param)
        .ok_or_else(|| RuntimeError::RelationParametersMissing {
            params: vec![cap_param.into()],
        })?;
    Ok(plasm_core::binding_value_as_plasm_value(
        raw,
        schema.named_value(cgs)?,
    )?)
}

/// Structural identity text is encoded only after typed parent-field restoration.
pub(crate) fn chain_binding_value(
    entity: &CachedEntity,
    parent_def: &plasm_core::EntityDef,
    parent_field: &EntityFieldName,
    cgs: &CGS,
) -> Result<String, RuntimeError> {
    let value = chain_binding_raw_json(entity, parent_def, parent_field, cgs)?;
    match value {
        serde_json::Value::String(text) if !text.is_empty() => Ok(text),
        value @ (serde_json::Value::Number(_) | serde_json::Value::Bool(_)) => {
            Ok(value.to_string())
        }
        _ => Err(RuntimeError::MaterializeBindingMissing {
            entity: parent_def.name.to_string(),
            field: parent_field.to_string(),
        }),
    }
}

#[cfg(test)]
mod sharing_tests {
    use super::*;

    #[test]
    fn cached_relation_occurrences_share_payloads_in_recorded_order() {
        let mut mat = SessionMaterialization::new();
        let refs = [Ref::new("Member", "a"), Ref::new("Member", "b")];
        for reference in &refs {
            mat.insert(CachedEntity::from_decoded(
                reference.clone(),
                IndexMap::new(),
                IndexMap::new(),
                0,
                crate::cache::EntityCompleteness::Complete,
            ))
            .unwrap();
        }
        let occurrences = [&refs[1], &refs[0], &refs[1]];
        let rows = resolve_cached_targets_from_relation_refs(&mat, occurrences, "Member").unwrap();
        assert_eq!(
            rows.iter().map(|row| &row.reference).collect::<Vec<_>>(),
            occurrences
        );
        assert!(std::ptr::eq(&rows[0], &rows[2]));
        for row in &rows {
            assert!(std::ptr::eq(row, mat.get(&row.reference).unwrap()));
        }
        assert!(resolve_cached_targets_from_relation_refs(&mat, [&refs[0]], "Other").is_err());
        assert!(resolve_cached_targets_from_relation_refs(
            &mat,
            [&Ref::new("Member", "missing")],
            "Member"
        )
        .is_err());
        drop(mat);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[2].reference, refs[1]);
    }
}
