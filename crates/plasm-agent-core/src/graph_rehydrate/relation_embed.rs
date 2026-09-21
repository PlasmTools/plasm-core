//! CEP-4: relation embed snapshot — one graph lock, then lock-free apply.

use indexmap::IndexMap;
use plasm_core::{
    partition_prefer_resolutions, Cardinality, Ref, RelationMaterialization, RelationRowResolution,
    CGS, MAX_FROM_PARENT_GET_EMBED_DEPTH,
};
use plasm_runtime::{entity_to_row_json, CachedEntity, SessionMaterialization};

use crate::execute_session::ExecuteSession;

fn relation_is_embed_materialize(materialize: &Option<RelationMaterialization>) -> bool {
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
        let Some(e) = (if r == root { Some(entity) } else { mat.get(&r) }) else {
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
        let Some(e) = (if r == root { Some(entity) } else { mat.get(&r) }) else {
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
                            obj.insert(wire.to_string(), embedded_identity_row(child, &memo, cgs));
                        }
                    }
                    Cardinality::Many => {
                        let arr: Vec<_> = refs
                            .iter()
                            .map(|child| embedded_identity_row(child, &memo, cgs))
                            .collect();
                        obj.insert(wire.to_string(), serde_json::Value::Array(arr));
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

/// A missing cache payload is still a known identity, never a display string or a dropped row.
fn embedded_identity_row(
    reference: &Ref,
    observed: &IndexMap<Ref, serde_json::Value>,
    cgs: &CGS,
) -> serde_json::Value {
    observed.get(reference).cloned().unwrap_or_else(|| {
        plasm_core::row_contract::RowCodec::new(Some(cgs)).identity_row(reference)
    })
}

fn identity_only_entity(reference: &Ref) -> CachedEntity {
    CachedEntity {
        reference: reference.clone(),
        fields: Default::default(),
        relations: Default::default(),
        last_updated: 0,
        version: 0,
        completeness: plasm_runtime::EntityCompleteness::Summary,
        unavailable_fields: Default::default(),
    }
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
            out.push(
                graph
                    .get(r)
                    .cloned()
                    .unwrap_or_else(|| identity_only_entity(r)),
            );
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{EmbedOnMissPolicy, JsonPathSegment, RelationScopedFallback};

    #[test]
    fn shuttle_relation_refs_survive_concurrent_child_eviction() {
        shuttle::check_random(
            || {
                let reference = Ref::new("Child", "c");
                let child = CachedEntity {
                    reference: reference.clone(),
                    fields: Default::default(),
                    relations: Default::default(),
                    last_updated: 0,
                    version: 0,
                    completeness: plasm_runtime::EntityCompleteness::Complete,
                    unavailable_fields: Default::default(),
                };
                let parent = CachedEntity {
                    reference: Ref::new("Parent", "p"),
                    fields: Default::default(),
                    relations: IndexMap::from([("children".into(), vec![reference.clone()])]),
                    last_updated: 0,
                    version: 0,
                    completeness: plasm_runtime::EntityCompleteness::Complete,
                    unavailable_fields: Default::default(),
                };
                let mut graph = SessionMaterialization::new();
                graph.merge_graph(vec![child.clone()]).unwrap();
                let graph = shuttle::sync::Arc::new(shuttle::sync::Mutex::new(graph));
                let writer_graph = graph.clone();
                let writer_ref = reference.clone();
                let writer = shuttle::thread::spawn(move || {
                    writer_graph.lock().unwrap().remove(&writer_ref);
                    shuttle::thread::yield_now();
                    writer_graph
                        .lock()
                        .unwrap()
                        .merge_graph(vec![child])
                        .unwrap();
                });
                let reader = shuttle::thread::spawn(move || {
                    for _ in 0..3 {
                        let rows = collect_all_embedded_relation_targets(
                            "children",
                            "Child",
                            &[parent.clone()],
                            &graph.lock().unwrap(),
                        )
                        .unwrap();
                        assert_eq!(rows.len(), 1);
                        assert_eq!(rows[0].reference, reference);
                        shuttle::thread::yield_now();
                    }
                });
                writer.join().unwrap();
                reader.join().unwrap();
            },
            500,
        );
    }

    proptest::proptest! {
        #[test]
        fn relation_refs_survive_arbitrary_child_cache_eviction(
            present in proptest::collection::vec(proptest::bool::ANY, 0..40)
        ) {
            let refs: Vec<_> = (0..present.len()).map(|i| Ref::new("Child", i.to_string())).collect();
            let parent = CachedEntity {
                reference: Ref::new("Parent", "p"),
                fields: Default::default(),
                relations: IndexMap::from([("children".into(), refs.clone())]),
                last_updated: 0, version: 0,
                completeness: plasm_runtime::EntityCompleteness::Complete,
                unavailable_fields: Default::default(),
            };
            let mut graph = SessionMaterialization::new();
            for (reference, exists) in refs.iter().zip(&present) {
                if *exists {
                    graph.merge_graph(vec![CachedEntity {
                        reference: reference.clone(),
                        fields: Default::default(), relations: Default::default(),
                        last_updated: 9, version: 0,
                        completeness: plasm_runtime::EntityCompleteness::Complete,
                        unavailable_fields: Default::default(),
                    }]).unwrap();
                }
            }
            let rows = collect_all_embedded_relation_targets("children", "Child", &[parent.clone()], &graph).unwrap();
            proptest::prop_assert_eq!(rows.iter().map(|r| r.reference.clone()).collect::<Vec<_>>(), refs);
            for (row, exists) in rows.iter().zip(&present) {
                proptest::prop_assert_eq!(row.completeness, if *exists { plasm_runtime::EntityCompleteness::Complete } else { plasm_runtime::EntityCompleteness::Summary });
            }
            proptest::prop_assert!(collect_all_embedded_relation_targets("missing", "Child", &[parent.clone()], &graph).is_none());
            if !present.is_empty() {
                proptest::prop_assert!(collect_all_embedded_relation_targets("children", "Other", &[parent], &graph).is_none());
            }
        }
    }

    proptest::proptest! {
        #[test]
        fn wire_embed_preserves_all_identities_under_partial_cache(
            present in proptest::collection::vec(proptest::bool::ANY, 1..16),
            root_cached in proptest::bool::ANY,
        ) {
            static MATRIX: std::sync::OnceLock<CGS> = std::sync::OnceLock::new();
            let cgs = MATRIX.get_or_init(|| plasm_core::load_schema_dir(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/schemas/plasm_language_matrix"),
            ).unwrap());
            let refs: Vec<_> = (0..present.len())
                .map(|i| Ref::new("LangLine", format!("line:{i}"))).collect();
            let parent = CachedEntity {
                reference: Ref::new("LangItem", "parent"),
                fields: Default::default(),
                relations: IndexMap::from([("lines".into(), refs.clone())]),
                last_updated: 0, version: 0,
                completeness: plasm_runtime::EntityCompleteness::Complete,
                unavailable_fields: Default::default(),
            };
            let mut graph = SessionMaterialization::new();
            if root_cached { graph.merge_graph(vec![parent.clone()]).unwrap(); }
            for (reference, cached) in refs.iter().zip(&present) {
                if *cached {
                    graph.merge_graph(vec![CachedEntity {
                        reference: reference.clone(), fields: Default::default(),
                        relations: Default::default(), last_updated: 0, version: 0,
                        completeness: plasm_runtime::EntityCompleteness::Complete,
                        unavailable_fields: Default::default(),
                    }]).unwrap();
                }
            }
            let row = wire_row_with_from_parent_embeds(&parent, cgs, &graph);
            let wire: serde_json::Value = serde_json::from_slice(&serde_json::to_vec(&row).unwrap()).unwrap();
            let children = wire["lines"].as_array().unwrap();
            proptest::prop_assert_eq!(children.len(), refs.len());
            for (child, reference) in children.iter().zip(refs) {
                proptest::prop_assert_eq!(child["id"].as_str().map(str::to_owned), Some(reference.primary_slot_str()));
                proptest::prop_assert_eq!(plasm_core::RefWire::parse_json(&child["_ref"]), Some(reference));
            }
        }
    }

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
            unavailable_fields: Default::default(),
        };
        let detail = CachedEntity {
            reference: Ref::new("LangDetail", "det-i1"),
            fields: indexmap::IndexMap::new(),
            relations: indexmap::IndexMap::new(),
            last_updated: 0,
            version: 0,
            completeness: plasm_runtime::EntityCompleteness::Complete,
            unavailable_fields: Default::default(),
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
