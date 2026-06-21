//! CEP-4: relation embed snapshot — one graph lock, then lock-free apply.

use indexmap::IndexMap;
use plasm_core::{
    partition_prefer_resolutions, Cardinality, Ref, RelationMaterialization, RelationRowResolution,
    CGS, MAX_FROM_PARENT_GET_EMBED_DEPTH,
};
use plasm_runtime::{CachedEntity, SessionMaterialization, entity_to_row_json};

use crate::execute_session::ExecuteSession;

fn relation_is_embed_materialize(
    materialize: &Option<RelationMaterialization>,
) -> bool {
    matches!(
        materialize,
        Some(RelationMaterialization::FromParentGet { .. })
            | Some(RelationMaterialization::PreferFromParentGet { .. })
    )
}

/// Agent wire row with `from_parent_get` relation objects embedded from the session graph.
/// Bounded iterative closure (deepest refs first) — no recursive stack growth (CEP-10).
pub(crate) fn wire_row_with_from_parent_embeds(
    entity: &CachedEntity,
    cgs: &CGS,
    mat: &SessionMaterialization,
) -> serde_json::Value {
    let root = entity.reference.clone();
    let mut depths: IndexMap<Ref, usize> = IndexMap::new();
    let mut queue = vec![(root.clone(), 0usize)];
    while let Some((r, depth)) = queue.pop() {
        if depth > MAX_FROM_PARENT_GET_EMBED_DEPTH || depths.contains_key(&r) {
            continue;
        }
        depths.insert(r.clone(), depth);
        let Some(e) = mat.get(&r) else {
            continue;
        };
        let Some(def) = cgs.get_entity(e.reference.entity_type.as_str()) else {
            continue;
        };
        for (rel_name, rel_schema) in &def.relations {
            if !relation_is_embed_materialize(&rel_schema.materialize) {
                continue;
            }
            let Some(refs) = e.relations.get(rel_name.as_str()) else {
                continue;
            };
            for child in refs {
                if !depths.contains_key(child) {
                    queue.push((child.clone(), depth + 1));
                }
            }
        }
    }

    let mut memo: IndexMap<Ref, serde_json::Value> = IndexMap::new();
    let mut refs_by_depth: Vec<(Ref, usize)> = depths.into_iter().collect();
    refs_by_depth.sort_by(|(_, a), (_, b)| b.cmp(a));

    for (r, _) in refs_by_depth {
        let Some(e) = mat.get(&r) else {
            continue;
        };
        let mut row = entity_to_row_json(e, Some(cgs));
        if let (Some(obj), Some(def)) = (
            row.as_object_mut(),
            cgs.get_entity(e.reference.entity_type.as_str()),
        ) {
            for (rel_name, rel_schema) in &def.relations {
                if !relation_is_embed_materialize(&rel_schema.materialize) {
                    continue;
                }
                let Some(refs) = e.relations.get(rel_name.as_str()) else {
                    continue;
                };
                let wire = rel_name.as_str();
                match rel_schema.cardinality {
                    Cardinality::One => {
                        if let Some(child) = refs.first() {
                            if let Some(child_json) = memo.get(child) {
                                obj.insert(wire.to_string(), child_json.clone());
                            }
                        }
                    }
                    Cardinality::Many => {
                        let arr: Vec<_> = refs
                            .iter()
                            .filter_map(|child| memo.get(child).cloned())
                            .collect();
                        if !arr.is_empty() {
                            obj.insert(wire.to_string(), serde_json::Value::Array(arr));
                        }
                    }
                }
            }
        }
        memo.insert(r, row);
    }

    memo.get(&root)
        .cloned()
        .unwrap_or_else(|| entity_to_row_json(entity, Some(cgs)))
}

/// Wire rows for materialized relation targets (full embed closure from session graph).
pub(crate) fn wire_rows_for_embed_entities(
    entities: &[CachedEntity],
    cgs: &CGS,
    mat: &SessionMaterialization,
) -> Vec<serde_json::Value> {
    entities
        .iter()
        .map(|e| wire_row_with_from_parent_embeds(e, cgs, mat))
        .collect()
}

/// Embed partition captured under one graph lock; consumed without lock.
pub(crate) struct RelationEmbedSnapshot {
    pub resolutions: Vec<RelationRowResolution>,
    pub embedded_per_row: Vec<Vec<CachedEntity>>,
    /// Fast path when every parent row is fully embedded in the session graph.
    pub all_embedded: Option<Vec<CachedEntity>>,
}

/// Single lock: resolutions + cloned embed targets.
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
            let Some(entity) = mat.get(r) else {
                continue;
            };
            embedded_per_row[row_index].push(entity.clone());
            if let Some(all) = &mut all_embedded {
                all.push(entity.clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{EmbedOnMissPolicy, JsonPathSegment, RelationScopedFallback};

    #[test]
    fn wire_row_embeds_declared_relation_from_graph() {
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("langmatrix");
        let summary = CachedEntity {
            reference: Ref::new("LangSummary", "sum-i1"),
            fields: indexmap::IndexMap::new(),
            relations: indexmap::IndexMap::from([(
                "detail".into(),
                vec![Ref::new("LangDetail", "det-i1")],
            )]),
            last_updated: 0,
            version: 0,
            completeness: plasm_runtime::EntityCompleteness::Complete,
        };
        let detail = CachedEntity {
            reference: Ref::new("LangDetail", "det-i1"),
            fields: indexmap::IndexMap::new(),
            relations: indexmap::IndexMap::new(),
            last_updated: 0,
            version: 0,
            completeness: plasm_runtime::EntityCompleteness::Complete,
        };
        let mut graph = SessionMaterialization::new();
        graph
            .merge_graph(vec![summary.clone(), detail.clone()])
            .expect("seed graph");

        let row = wire_row_with_from_parent_embeds(&summary, &cgs, &graph);
        assert!(
            row.get("detail").is_some(),
            "wire row must embed the declared relation hop"
        );
    }

    #[test]
    fn prefer_projected_parent_relation_refs_without_graph_falls_back_scoped() {
        let mat = RelationMaterialization::PreferFromParentGet {
            path: vec![
                JsonPathSegment::Key {
                    key: "pokemon".into(),
                },
                JsonPathSegment::Wildcard { wildcard: true },
                JsonPathSegment::Key {
                    key: "pokemon".into(),
                },
            ],
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::HydrateFromEmbedPath {
                path: Vec::new(),
                get_capability: "pokemon_get".into(),
            },
        };
        let refs = vec![Ref::new("Pokemon", "jolteon")];
        let projected = serde_json::json!({"name": "electric"});
        let resolutions = partition_prefer_resolutions(
            &mat,
            "pokemon",
            "Pokemon",
            [(&projected, Some(refs.as_slice()))],
            |_| false,
        );
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0], RelationRowResolution::ScopedQuery);
    }

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
