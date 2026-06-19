//! CEP-4: relation embed snapshot — one graph lock, then lock-free apply.
//!
//! [`RelationEmbedSnapshot`] mirrors [`super::rehydrator::GraphSpillSyncPlan`]: plan under lock,
//! HTTP/spill apply without lock.

use std::collections::HashSet;

use indexmap::IndexMap;
use plasm_core::{
    partition_prefer_resolutions, Cardinality, Ref, RelationMaterialization, RelationRowResolution,
    CGS,
};
use plasm_runtime::{CachedEntity, SessionMaterialization};

use crate::execute_session::ExecuteSession;

/// Embed partition captured under one graph lock; consumed without lock.
pub(crate) struct RelationEmbedSnapshot {
    pub resolutions: Vec<RelationRowResolution>,
    pub embedded_per_row: Vec<Vec<CachedEntity>>,
    /// Fast path when every parent row is fully embedded in the session graph.
    pub all_embedded: Option<Vec<CachedEntity>>,
}

/// Frozen ref → entity map for nested wire-row rebuild (no live graph access).
#[derive(Default, Clone)]
pub(crate) struct RefEmbedLookup {
    entities: IndexMap<Ref, CachedEntity>,
}

impl RefEmbedLookup {
    pub(crate) fn insert(&mut self, reference: Ref, entity: CachedEntity) {
        self.entities.insert(reference, entity);
    }

    pub(crate) fn get(&self, reference: &Ref) -> Option<&CachedEntity> {
        self.entities.get(reference)
    }

    pub(crate) fn materialization_view(&self) -> EmbedLookupMaterialization<'_> {
        EmbedLookupMaterialization { lookup: self }
    }
}

pub(crate) struct EmbedLookupMaterialization<'a> {
    lookup: &'a RefEmbedLookup,
}

impl EmbedLookupMaterialization<'_> {
    pub(crate) fn get(&self, reference: &Ref) -> Option<&CachedEntity> {
        self.lookup.get(reference)
    }
}

/// Single lock: resolutions + cloned embed targets (+ lookup for nested wire rows).
pub(crate) async fn plan_prefer_from_parent_get(
    scoped_es: &ExecuteSession,
    materialize: &RelationMaterialization,
    rel_name: &str,
    target_entity: &str,
    parents: &[CachedEntity],
    source_rows: &[serde_json::Value],
) -> Result<RelationEmbedSnapshot, String> {
    if parents.len() != source_rows.len() {
        return Err(format!(
            "prefer embed plan: parent count {} != source row count {}",
            parents.len(),
            source_rows.len()
        ));
    }
    let parent_rows: Vec<(&serde_json::Value, Option<&[Ref]>)> = source_rows
        .iter()
        .enumerate()
        .map(|(row_index, row)| {
            (
                row,
                parents
                    .get(row_index)
                    .and_then(|p| p.relations.get(rel_name).map(|v| v.as_slice())),
            )
        })
        .collect();

    let guard = scoped_es.lock_graph_cache().await;
    let mat = guard.materialization();
    let resolutions =
        partition_prefer_resolutions(materialize, rel_name, target_entity, parent_rows, |r| {
            mat.get(r).is_some()
        });
    let row_count = resolutions.len();
    let mut embedded_per_row = vec![Vec::new(); row_count];
    let all_rows_embedded = resolutions
        .iter()
        .all(|r| matches!(r, RelationRowResolution::EmbeddedRefs(_)));
    let mut all_embedded = if all_rows_embedded {
        Some(Vec::new())
    } else {
        None
    };
    for (row_index, resolution) in resolutions.iter().enumerate() {
        let RelationRowResolution::EmbeddedRefs(refs) = resolution else {
            continue;
        };
        for r in refs {
            let entity = mat
                .get(r)
                .ok_or_else(|| format!("prefer embed plan: missing graph target {r}"))?
                .clone();
            embedded_per_row[row_index].push(entity.clone());
            if let Some(all) = &mut all_embedded {
                all.push(entity);
            }
        }
    }
    drop(guard);

    Ok(RelationEmbedSnapshot {
        resolutions,
        embedded_per_row,
        all_embedded,
    })
}

/// When every parent row has fully resolved embed refs in the session graph.
pub(crate) fn collect_all_embedded_relation_targets(
    relation_name: &str,
    target_entity: &str,
    parents: &[CachedEntity],
    graph: &SessionMaterialization,
) -> Option<Vec<CachedEntity>> {
    let mut out = Vec::new();
    for parent in parents {
        if !parent.relations.contains_key(relation_name) {
            return None;
        }
        let refs = parent.relations.get(relation_name)?;
        for r in refs {
            if r.entity_type.as_str() != target_entity {
                return None;
            }
            out.push(graph.get(r)?.clone());
        }
    }
    Some(out)
}

fn relation_materialize_is_parent_get(mat: &Option<RelationMaterialization>) -> bool {
    matches!(
        mat,
        Some(RelationMaterialization::FromParentGet { .. })
            | Some(RelationMaterialization::PreferFromParentGet { .. })
    )
}

/// Rebuild wire rows from snapshot-resolved targets, nesting declared `from_parent_get` embeds.
pub(crate) fn wire_rows_with_nested_relation_embeds(
    entities: &[CachedEntity],
    entity_type: &str,
    cgs: &CGS,
    graph: &EmbedLookupMaterialization<'_>,
) -> Vec<serde_json::Value> {
    use plasm_runtime::entity_to_row_json;

    let Some(def) = cgs.get_entity(entity_type) else {
        return entities
            .iter()
            .map(|e| entity_to_row_json(e, Some(cgs)))
            .collect();
    };
    entities
        .iter()
        .map(|entity| {
            let mut row = entity_to_row_json(entity, Some(cgs));
            let Some(obj) = row.as_object_mut() else {
                return row;
            };
            for (rel_name, rel_schema) in &def.relations {
                if !relation_materialize_is_parent_get(&rel_schema.materialize) {
                    continue;
                }
                let Some(refs) = entity.relations.get(rel_name.as_str()) else {
                    continue;
                };
                let target = rel_schema.target_resource.as_str();
                match rel_schema.cardinality {
                    Cardinality::One => {
                        if let Some(child) = refs.first().and_then(|r| graph.get(r)) {
                            let nested = wire_rows_with_nested_relation_embeds(
                                std::slice::from_ref(child),
                                target,
                                cgs,
                                graph,
                            );
                            if let Some(n) = nested.into_iter().next() {
                                obj.insert(rel_name.as_str().to_string(), n);
                            }
                        }
                    }
                    Cardinality::Many => {
                        let arr: Vec<_> = refs
                            .iter()
                            .filter_map(|r| graph.get(r))
                            .flat_map(|child| {
                                wire_rows_with_nested_relation_embeds(
                                    std::slice::from_ref(child),
                                    target,
                                    cgs,
                                    graph,
                                )
                            })
                            .collect();
                        if !arr.is_empty() {
                            obj.insert(
                                rel_name.as_str().to_string(),
                                serde_json::Value::Array(arr),
                            );
                        }
                    }
                }
            }
            row
        })
        .collect()
}

/// Brief lock: collect embedded targets + extend lookup with nested refs for wire rows.
pub(crate) async fn snapshot_cached_embed_targets(
    scoped_es: &ExecuteSession,
    rel_name: &str,
    target_entity: &str,
    parents: &[CachedEntity],
) -> Result<Option<(Vec<CachedEntity>, RefEmbedLookup)>, String> {
    let guard = scoped_es.lock_graph_cache().await;
    let Some(entities) =
        collect_all_embedded_relation_targets(rel_name, target_entity, parents, &guard)
    else {
        return Ok(None);
    };
    let mut embed_lookup = RefEmbedLookup::default();
    for entity in &entities {
        embed_lookup.insert(entity.reference.clone(), entity.clone());
    }
    extend_lookup_nested_embeds(
        &mut embed_lookup,
        &entities,
        target_entity,
        &guard,
        scoped_es.cgs.as_ref(),
    );
    drop(guard);
    Ok(Some((entities, embed_lookup)))
}

fn extend_lookup_nested_embeds(
    lookup: &mut RefEmbedLookup,
    entities: &[CachedEntity],
    entity_type: &str,
    graph: &SessionMaterialization,
    cgs: &CGS,
) {
    let Some(def) = cgs.get_entity(entity_type) else {
        return;
    };
    let mut pending: Vec<Ref> = Vec::new();
    for entity in entities {
        for (rel_name, rel_schema) in &def.relations {
            if !relation_materialize_is_parent_get(&rel_schema.materialize) {
                continue;
            }
            if let Some(refs) = entity.relations.get(rel_name.as_str()) {
                for r in refs {
                    if lookup.get(r).is_none() {
                        pending.push(r.clone());
                    }
                }
            }
        }
    }
    let mut seen = HashSet::new();
    pending.retain(|r| seen.insert(r.clone()));
    let mut queue = pending;
    while let Some(r) = queue.pop() {
        if lookup.get(&r).is_some() {
            continue;
        }
        let Some(entity) = graph.get(&r).cloned() else {
            continue;
        };
        let nested_type = entity.reference.entity_type.as_str();
        lookup.insert(r, entity.clone());
        if let Some(nested_def) = cgs.get_entity(nested_type) {
            for (rel_name, rel_schema) in &nested_def.relations {
                if !relation_materialize_is_parent_get(&rel_schema.materialize) {
                    continue;
                }
                if let Some(refs) = entity.relations.get(rel_name.as_str()) {
                    for child_ref in refs {
                        if lookup.get(child_ref).is_none() {
                            queue.push(child_ref.clone());
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{EmbedOnMissPolicy, JsonPathSegment, RelationScopedFallback};

    #[test]
    fn partition_prefer_resolutions_matches_row_resolution() {
        let mat = RelationMaterialization::PreferFromParentGet {
            path: vec![JsonPathSegment::Key { key: "tags".into() }],
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::QueryScoped {
                capability: "cap".into(),
                param: "p".into(),
            },
        };
        let row = serde_json::json!({"tags": [{"id": 1}]});
        let refs = vec![Ref::new("Tag", "1")];
        let resolutions = partition_prefer_resolutions(
            &mat,
            "tags",
            "Tag",
            [(&row, Some(refs.as_slice()))],
            |_| true,
        );
        assert_eq!(resolutions.len(), 1);
        assert!(matches!(
            resolutions[0],
            RelationRowResolution::EmbeddedRefs(_)
        ));
    }
}
