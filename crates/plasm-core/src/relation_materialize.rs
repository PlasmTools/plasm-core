//! Typed per-parent relation row resolution (CGS `materialize` → embed vs scoped HTTP).
//!
//! Plan and runtime share this module so strategy does not depend on cache-shape heuristics.
//!
//! **Typed row invariant:** relation traversals must yield rows shaped as the target CGS entity
//! (full field set or explicit nulls). Wire embeds are transport shortcuts; incomplete embeds
//! are hydrated via target GET before plan compute (see `plasm_plan_run::relation_hydrate`).

use crate::{Cardinality, EmbedOnMissPolicy, JsonPathSegment, Ref, RelationMaterialization};
use crate::{Value, ValueRow};

/// Max depth for chained `from_parent_get` embed decode, graph insert, and wire-row rebuild (CEP-10).
pub const MAX_FROM_PARENT_GET_EMBED_DEPTH: usize = 8;

/// Whether a parent row is served from the session graph or needs a scoped query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationRowResolution {
    /// Use decoded relation refs (caller resolves against graph / materializes JSON).
    EmbeddedRefs(Vec<Ref>),
    /// Run the catalog-declared scoped query for this parent row.
    ScopedQuery,
}

/// Extract nested JSON values along a `from_parent_get` path.
pub fn extract_from_parent_get_value(row: &Value, path: &[JsonPathSegment]) -> Vec<Value> {
    fn walk(cur: &Value, path: &[JsonPathSegment], idx: usize) -> Vec<Value> {
        if idx >= path.len() {
            return vec![cur.clone()];
        }
        match &path[idx] {
            JsonPathSegment::Key { key } => cur
                .get(key.as_str())
                .map(|next| walk(next, path, idx + 1))
                .unwrap_or_default(),
            JsonPathSegment::Wildcard { wildcard: true } => cur
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .flat_map(|item| walk(item, path, idx + 1))
                        .collect()
                })
                .unwrap_or_default(),
            JsonPathSegment::Wildcard { wildcard: false } => Vec::new(),
        }
    }
    walk(row, path, 0)
}

/// Count exhaustive path leaves; explicit empty arrays are evidence, missing/null paths are not.
/// Checks every wildcard branch, including suffixes after the wildcard.
pub fn exhaustive_from_parent_get_count(value: &Value, path: &[JsonPathSegment]) -> Option<usize> {
    let Some((segment, tail)) = path.split_first() else {
        return (!value.is_null()).then_some(1);
    };
    match segment {
        JsonPathSegment::Key { key } => {
            exhaustive_from_parent_get_count(value.get(key.as_str())?, tail)
        }
        JsonPathSegment::Wildcard { wildcard: true } => {
            value.as_array()?.iter().try_fold(0usize, |count, item| {
                count.checked_add(exhaustive_from_parent_get_count(item, tail)?)
            })
        }
        JsonPathSegment::Wildcard { wildcard: false } => None,
    }
}

/// Returns true when every ref is present in `get` and has the expected target entity type.
pub fn relation_refs_fully_resolved<'a>(
    refs: &[Ref],
    expected_target: &str,
    mut get: impl FnMut(&Ref) -> Option<&'a ()>,
) -> bool {
    refs.iter()
        .all(|r| r.entity_type.as_str() == expected_target && get(r).is_some())
}

/// Decide embed vs scoped query for one parent row from frozen catalog materialization.
pub fn resolve_relation_row_resolution(
    materialize: &RelationMaterialization,
    cardinality: crate::Cardinality,
    relation_name: &str,
    expected_target: &str,
    parent_json: &Value,
    relation_refs: Option<&[Ref]>,
    graph_has_ref: impl Fn(&Ref) -> bool,
) -> RelationRowResolution {
    match materialize {
        RelationMaterialization::Unavailable => RelationRowResolution::ScopedQuery,
        RelationMaterialization::FromParentGet {
            collection_coverage,
            ..
        } if cardinality == crate::Cardinality::One
            || *collection_coverage == crate::EmbeddedCollectionCoverage::Complete =>
        {
            RelationRowResolution::EmbeddedRefs(
                relation_refs.map(|s| s.to_vec()).unwrap_or_default(),
            )
        }
        RelationMaterialization::FromParentGet { .. } => RelationRowResolution::ScopedQuery,
        RelationMaterialization::QueryScoped { .. }
        | RelationMaterialization::QueryScopedBindings { .. } => RelationRowResolution::ScopedQuery,
        RelationMaterialization::GetScopedBindings { .. } => RelationRowResolution::ScopedQuery,
        RelationMaterialization::ViewEmbed { .. } => RelationRowResolution::EmbeddedRefs(
            relation_refs.map(|s| s.to_vec()).unwrap_or_default(),
        ),
        RelationMaterialization::PreferFromParentGet {
            path,
            collection_coverage: crate::EmbeddedCollectionCoverage::Complete,
            on_embed_miss,
            fallback: _,
        } if cardinality == crate::Cardinality::Many => resolve_prefer_from_parent_get_row(
            path,
            *on_embed_miss,
            relation_name,
            expected_target,
            parent_json,
            relation_refs,
            graph_has_ref,
        ),
        RelationMaterialization::PreferFromParentGet {
            path,
            on_embed_miss,
            ..
        } if cardinality == crate::Cardinality::One => resolve_prefer_from_parent_get_row(
            path,
            *on_embed_miss,
            relation_name,
            expected_target,
            parent_json,
            relation_refs,
            graph_has_ref,
        ),
        RelationMaterialization::PreferFromParentGet { .. } => RelationRowResolution::ScopedQuery,
    }
}

fn resolve_prefer_from_parent_get_row(
    path: &[JsonPathSegment],
    on_embed_miss: EmbedOnMissPolicy,
    _relation_name: &str,
    expected_target: &str,
    parent_json: &Value,
    relation_refs: Option<&[Ref]>,
    graph_has_ref: impl Fn(&Ref) -> bool,
) -> RelationRowResolution {
    if let Some(refs) = relation_refs.filter(|r| !r.is_empty()) {
        if relation_refs_fully_resolved(refs, expected_target, |r| graph_has_ref(r).then_some(&()))
        {
            return RelationRowResolution::EmbeddedRefs(refs.to_vec());
        }
    }

    let extracted = extract_from_parent_get_value(parent_json, path);
    let path_empty = extracted.is_empty() || extracted.iter().all(|v| v.is_null());
    if path_empty {
        if let Some(refs) = relation_refs.filter(|r| !r.is_empty()) {
            if relation_refs_fully_resolved(refs, expected_target, |r| {
                graph_has_ref(r).then_some(&())
            }) {
                return RelationRowResolution::EmbeddedRefs(refs.to_vec());
            }
        }
        return RelationRowResolution::ScopedQuery;
    }

    match on_embed_miss {
        EmbedOnMissPolicy::FallbackScoped => RelationRowResolution::ScopedQuery,
    }
}

/// Per-parent embed vs scoped resolution for plan/runtime prefer materialization.
pub fn partition_prefer_resolutions<'a, F>(
    materialize: &RelationMaterialization,
    cardinality: crate::Cardinality,
    relation_key: &str,
    expected_target: &str,
    parent_rows: impl IntoIterator<Item = (&'a Value, Option<&'a [Ref]>)>,
    graph_has_ref: F,
) -> Vec<RelationRowResolution>
where
    F: Fn(&Ref) -> bool,
{
    parent_rows
        .into_iter()
        .map(|(parent_json, relation_refs)| {
            resolve_relation_row_resolution(
                materialize,
                cardinality,
                relation_key,
                expected_target,
                parent_json,
                relation_refs,
                &graph_has_ref,
            )
        })
        .collect()
}

/// Directed edges `(source_entity, relation) → target_entity` for embed materialization strategies.
pub fn from_parent_get_embed_edges(cgs: &crate::CGS) -> Vec<(String, String, String)> {
    use crate::RelationMaterialization;

    let mut edges = Vec::new();
    for (entity_name, entity) in &cgs.entities {
        for (relation_name, relation) in &entity.relations {
            let Some(target) = (match &relation.materialize {
                Some(RelationMaterialization::FromParentGet { .. }) => {
                    Some(relation.target_resource.to_string())
                }
                _ => None,
            }) else {
                continue;
            };
            edges.push((entity_name.to_string(), relation_name.to_string(), target));
        }
    }
    edges
}

/// Fail when plain [`RelationMaterialization::FromParentGet`] edges form an entity-level cycle.
///
/// [`RelationMaterialization::PreferFromParentGet`] inverse edges are excluded — mutual embed
/// pairs are allowed when runtime decode uses leaf embed decoders (CEP-10).
pub fn validate_from_parent_get_embed_acyclic(cgs: &crate::CGS) -> Result<(), EntityCycle> {
    let edges = from_parent_get_embed_edges(cgs);
    let mut adj: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    for (from, _rel, to) in &edges {
        // Self-embed (e.g. HN Item.kids → Item) is single-hop safe; skip for cycle detection.
        if from == to {
            continue;
        }
        adj.entry(from.clone()).or_default().push(to.clone());
    }
    for start in adj.keys().cloned().collect::<Vec<_>>() {
        if let Some(cycle) = find_entity_cycle(&adj, &start) {
            return Err(EntityCycle { entities: cycle });
        }
    }
    Ok(())
}

/// Ordered entity path that closes a `from_parent_get` materialization cycle.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", .entities.join(" → "))]
pub struct EntityCycle {
    entities: Vec<String>,
}

impl EntityCycle {
    pub fn entities(&self) -> &[String] {
        &self.entities
    }
}

pub(crate) fn find_entity_cycle(
    adj: &std::collections::HashMap<String, Vec<String>>,
    start: &str,
) -> Option<Vec<String>> {
    fn dfs(
        adj: &std::collections::HashMap<String, Vec<String>>,
        node: &str,
        stack: &mut Vec<String>,
        on_stack: &mut std::collections::HashSet<String>,
        visited: &mut std::collections::HashSet<String>,
    ) -> Option<Vec<String>> {
        if on_stack.contains(node) {
            let pos = stack.iter().position(|n| n == node).unwrap_or(stack.len());
            let mut cycle = stack[pos..].to_vec();
            cycle.push(node.to_string());
            return Some(cycle);
        }
        if visited.contains(node) {
            return None;
        }
        visited.insert(node.to_string());
        on_stack.insert(node.to_string());
        stack.push(node.to_string());
        if let Some(nexts) = adj.get(node) {
            for next in nexts {
                if let Some(cycle) = dfs(adj, next, stack, on_stack, visited) {
                    return Some(cycle);
                }
            }
        }
        stack.pop();
        on_stack.remove(node);
        None
    }

    let mut stack = Vec::new();
    let mut on_stack = std::collections::HashSet::new();
    let mut visited = std::collections::HashSet::new();
    dfs(adj, start, &mut stack, &mut on_stack, &mut visited)
}

/// Flatten extracted path values across parent rows (plan materialize helper).
pub fn flatten_from_parent_get_source_rows(
    source_rows: &[ValueRow],
    path: &[JsonPathSegment],
    cardinality: Cardinality,
) -> Vec<Value> {
    let mut out = Vec::new();
    for row in source_rows {
        let extracted = extract_from_parent_get_value(row, path);
        match cardinality {
            Cardinality::One => {
                if let Some(v) = extracted.into_iter().next() {
                    if !v.is_null() {
                        out.push(v);
                    }
                }
            }
            Cardinality::Many => out.extend(extracted.into_iter().filter(|v| !v.is_null())),
        }
    }
    out
}

/// Embed path for [`RelationScopedFallback::HydrateFromEmbedPath`]: defaults to `prefer_path` when omitted.
pub fn prefer_hydrate_embed_path<'a>(
    prefer_path: &'a [JsonPathSegment],
    fallback: &'a crate::RelationScopedFallback,
) -> Option<&'a [JsonPathSegment]> {
    match fallback {
        crate::RelationScopedFallback::HydrateFromEmbedPath { path, .. } if path.is_empty() => {
            Some(prefer_path)
        }
        crate::RelationScopedFallback::HydrateFromEmbedPath { path, .. } => Some(path.as_slice()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RelationMaterialization, RelationScopedFallback};
    use indexmap::IndexMap;

    #[test]
    fn catalog_rejects_many_embeds_without_membership_evidence() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/from_parent_get_nav");
        let mut cgs = crate::load_schema_dir(&directory).unwrap();
        let relation = cgs
            .entities
            .get_mut("ParentItem")
            .unwrap()
            .relations
            .get_mut("tags")
            .unwrap();
        let Some(RelationMaterialization::FromParentGet {
            collection_coverage,
            ..
        }) = &mut relation.materialize
        else {
            panic!("fixture must exercise an embedded many-relation");
        };
        *collection_coverage = crate::EmbeddedCollectionCoverage::Unknown;
        assert!(
            matches!(cgs.validate(), Err(crate::SchemaError::RelationMembershipUnproven { entity, relation }) if entity == "ParentItem" && relation == "tags")
        );
    }

    #[test]
    fn catalog_rejects_preferred_many_embed_without_membership_evidence() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let mut cgs = crate::load_schema_dir(&directory).unwrap();
        let relation = cgs
            .entities
            .get_mut("LangItem")
            .unwrap()
            .relations
            .get_mut("tags")
            .unwrap();
        let Some(RelationMaterialization::PreferFromParentGet {
            collection_coverage,
            ..
        }) = &mut relation.materialize
        else {
            panic!("fixture must exercise a preferred embedded many-relation");
        };
        *collection_coverage = crate::EmbeddedCollectionCoverage::Unknown;
        assert!(
            matches!(cgs.validate(), Err(crate::SchemaError::RelationMembershipUnproven { entity, relation }) if entity == "LangItem" && relation == "tags")
        );
    }

    #[test]
    fn embedded_collection_coverage_serialization_is_explicit() {
        let absent = serde_json::json!({"kind":"from_parent_get", "path":[{"key":"items"},{"wildcard":true}]});
        let parsed: RelationMaterialization = serde_json::from_value(absent.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), absent);
        let complete = serde_json::json!({"kind":"from_parent_get", "collection_coverage":"complete", "path":[{"key":"items"},{"wildcard":true}]});
        let parsed: RelationMaterialization = serde_json::from_value(complete.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), complete);
        let invalid = serde_json::json!({"kind":"from_parent_get", "collection_coverage":"maybe", "path":[{"key":"items"}]});
        assert!(serde_json::from_value::<RelationMaterialization>(invalid).is_err());

        let unknown_prefer = serde_json::json!({
            "kind":"prefer_from_parent_get",
            "path":[{"key":"items"}],
            "on_embed_miss":"fallback_scoped",
            "fallback":{"kind":"query_scoped", "capability":"items_query", "param":"parent"}
        });
        let parsed: RelationMaterialization =
            serde_json::from_value(unknown_prefer.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), unknown_prefer);
        let complete_prefer = serde_json::json!({
            "kind":"prefer_from_parent_get",
            "path":[{"key":"items"}],
            "collection_coverage":"complete",
            "on_embed_miss":"fallback_scoped",
            "fallback":{"kind":"query_scoped", "capability":"items_query", "param":"parent"}
        });
        let parsed: RelationMaterialization =
            serde_json::from_value(complete_prefer.clone()).unwrap();
        assert_eq!(serde_json::to_value(&parsed).unwrap(), complete_prefer);
    }

    #[test]
    fn pure_query_scoped_always_scoped() {
        let mat = RelationMaterialization::QueryScopedBindings {
            capability: "cap".into(),
            bindings: IndexMap::new(),
        };
        let res = resolve_relation_row_resolution(
            &mat,
            Cardinality::Many,
            "tags",
            "Tag",
            &crate::fixture_value!({"tags": [{"id": 1}]}),
            Some(&[Ref::new("Tag", "1")]),
            |_| true,
        );
        assert_eq!(res, RelationRowResolution::ScopedQuery);
    }

    #[test]
    fn prefer_empty_path_extract_scoped() {
        let mat = RelationMaterialization::PreferFromParentGet {
            path: vec![JsonPathSegment::Key {
                key: "labels".into(),
            }],
            collection_coverage: crate::EmbeddedCollectionCoverage::Complete,
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::QueryScopedBindings {
                capability: "issue_label_query".into(),
                bindings: IndexMap::new(),
            },
        };
        let res = resolve_relation_row_resolution(
            &mat,
            Cardinality::Many,
            "labels",
            "Label",
            &crate::fixture_value!({"labels": []}),
            Some(&[]),
            |_| true,
        );
        assert_eq!(res, RelationRowResolution::ScopedQuery);
    }

    #[test]
    fn prefer_graph_miss_fallback_scoped() {
        let mat = RelationMaterialization::PreferFromParentGet {
            path: vec![
                JsonPathSegment::Key {
                    key: "labels".into(),
                },
                JsonPathSegment::Wildcard { wildcard: true },
            ],
            collection_coverage: crate::EmbeddedCollectionCoverage::Complete,
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::QueryScopedBindings {
                capability: "issue_label_query".into(),
                bindings: IndexMap::new(),
            },
        };
        let refs = vec![Ref::new("Label", "99")];
        let row = crate::fixture_value!({"labels": [{"id": 99}]});
        let resolutions = partition_prefer_resolutions(
            &mat,
            Cardinality::Many,
            "labels",
            "Label",
            [(&row, Some(refs.as_slice()))],
            |_| false,
        );
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0], RelationRowResolution::ScopedQuery);
    }

    #[test]
    fn prefer_graph_refs_when_projected_parent_strips_embed() {
        use crate::{EmbedOnMissPolicy, JsonPathSegment, RelationScopedFallback};

        let mat = RelationMaterialization::PreferFromParentGet {
            path: vec![
                JsonPathSegment::Key { key: "tags".into() },
                JsonPathSegment::Wildcard { wildcard: true },
            ],
            collection_coverage: crate::EmbeddedCollectionCoverage::Complete,
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::QueryScopedBindings {
                capability: "langtag_query".into(),
                bindings: IndexMap::new(),
            },
        };
        let refs = vec![Ref::new("LangTag", "t1")];
        let projected = crate::fixture_value!({"id": "i1", "title": "Demo"});
        let res = resolve_relation_row_resolution(
            &mat,
            Cardinality::Many,
            "tags",
            "LangTag",
            &projected,
            Some(&refs),
            |_| true,
        );
        assert_eq!(res, RelationRowResolution::EmbeddedRefs(refs));
    }

    #[test]
    fn prefer_unknown_collection_coverage_ignores_cached_refs() {
        let mat = RelationMaterialization::PreferFromParentGet {
            path: vec![
                JsonPathSegment::Key { key: "tags".into() },
                JsonPathSegment::Wildcard { wildcard: true },
            ],
            collection_coverage: crate::EmbeddedCollectionCoverage::Unknown,
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::QueryScoped {
                capability: "langtag_query".into(),
                param: "item_id".into(),
            },
        };
        let refs = vec![Ref::new("LangTag", "t1")];
        let parent = crate::fixture_value!({"id": "i1", "tags": [{"id": "t1"}]});
        assert_eq!(
            resolve_relation_row_resolution(
                &mat,
                Cardinality::Many,
                "tags",
                "LangTag",
                &parent,
                Some(&refs),
                |_| { true },
            ),
            RelationRowResolution::ScopedQuery
        );
    }

    #[test]
    fn unknown_singleton_prefer_preserves_cached_target() {
        let mat = RelationMaterialization::PreferFromParentGet {
            path: vec![JsonPathSegment::Key {
                key: "detail".into(),
            }],
            collection_coverage: crate::EmbeddedCollectionCoverage::Unknown,
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::QueryScoped {
                capability: "detail_query".into(),
                param: "parent_id".into(),
            },
        };
        let refs = vec![Ref::new("Detail", "d1")];
        assert_eq!(
            resolve_relation_row_resolution(
                &mat,
                Cardinality::One,
                "detail",
                "Detail",
                &crate::fixture_value!({"id": "p1"}),
                Some(&refs),
                |_| true,
            ),
            RelationRowResolution::EmbeddedRefs(refs)
        );
    }

    #[test]
    fn unknown_many_from_parent_get_fails_closed() {
        let mat = RelationMaterialization::FromParentGet {
            path: vec![JsonPathSegment::Key {
                key: "children".into(),
            }],
            collection_coverage: crate::EmbeddedCollectionCoverage::Unknown,
        };
        assert_eq!(
            resolve_relation_row_resolution(
                &mat,
                Cardinality::Many,
                "children",
                "Child",
                &crate::fixture_value!({"children": [{"id": "c1"}]}),
                Some(&[Ref::new("Child", "c1")]),
                |_| true,
            ),
            RelationRowResolution::ScopedQuery
        );
    }

    #[test]
    fn prefer_partial_graph_miss_falls_back_scoped() {
        use crate::{EmbedOnMissPolicy, JsonPathSegment, RelationScopedFallback};

        let mat = RelationMaterialization::PreferFromParentGet {
            path: vec![
                JsonPathSegment::Key { key: "tags".into() },
                JsonPathSegment::Wildcard { wildcard: true },
            ],
            collection_coverage: crate::EmbeddedCollectionCoverage::Complete,
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::QueryScopedBindings {
                capability: "langtag_query".into(),
                bindings: IndexMap::new(),
            },
        };
        let refs = vec![Ref::new("LangTag", "t1")];
        let row = crate::fixture_value!({"tags": [{"id": "t1"}]});
        let res = resolve_relation_row_resolution(
            &mat,
            Cardinality::Many,
            "tags",
            "LangTag",
            &row,
            Some(&refs),
            |_| false,
        );
        assert_eq!(res, RelationRowResolution::ScopedQuery);
    }

    #[test]
    fn from_parent_get_cycle_rejected() {
        use std::collections::HashMap;

        let mut adj: HashMap<String, Vec<String>> = HashMap::new();
        adj.insert("Pokemon".into(), vec!["Type".into()]);
        adj.insert("Type".into(), vec!["Pokemon".into()]);
        let cycle = find_entity_cycle(&adj, "Pokemon").expect("cycle");
        assert!(cycle.first().map(|s| s.as_str()) == Some("Pokemon"));
        assert!(cycle.contains(&"Type".to_string()));
    }

    #[test]
    fn prefer_type_pokemon_wire_embed_extracts_identities() {
        use crate::{EmbedOnMissPolicy, JsonPathSegment, RelationScopedFallback};

        let path = vec![
            JsonPathSegment::Key {
                key: "pokemon".into(),
            },
            JsonPathSegment::Wildcard { wildcard: true },
            JsonPathSegment::Key {
                key: "pokemon".into(),
            },
        ];
        let mat = RelationMaterialization::PreferFromParentGet {
            path: path.clone(),
            collection_coverage: crate::EmbeddedCollectionCoverage::Complete,
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::HydrateFromEmbedPath {
                path: path.clone(),
                get_capability: "pokemon_get".into(),
            },
        };
        let row = crate::fixture_value!({
            "name": "electric",
            "pokemon": [
                { "pokemon": { "name": "pikachu", "url": "https://pokeapi.co/api/v2/pokemon/25/" } }
            ]
        });
        let extracted = extract_from_parent_get_value(&row, &path);
        assert_eq!(extracted.len(), 1);
        assert_eq!(
            extracted[0].get("name").and_then(Value::as_str),
            Some("pikachu")
        );
        let res = resolve_relation_row_resolution(
            &mat,
            Cardinality::Many,
            "pokemon",
            "Pokemon",
            &row,
            None,
            |_| false,
        );
        assert_eq!(res, RelationRowResolution::ScopedQuery);
    }

    #[test]
    fn language_matrix_prefer_embed_loads_and_validates() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = crate::loader::load_schema(&dir).expect("plasm_language_matrix");
        cgs.validate()
            .expect("language matrix validates with prefer embeds");
        validate_from_parent_get_embed_acyclic(&cgs)
            .expect("forward from_parent_get edges acyclic");
        let tags_rel = cgs
            .get_entity("LangItem")
            .and_then(|e| e.relations.get("tags"))
            .expect("LangItem.tags");
        assert!(matches!(
            tags_rel.materialize,
            Some(RelationMaterialization::PreferFromParentGet { .. })
        ));
    }

    #[test]
    fn matrix_schema_fixtures_load_and_validate() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas");
        for name in [
            "plasm_language_matrix",
            "plasm_language_matrix_views",
            "plasm_prompt_matrix",
            "plasm_pagination_matrix",
            "pokeapi_mini",
            "overshow_tools",
        ] {
            let path = root.join(name);
            let cgs =
                crate::loader::load_schema(&path).unwrap_or_else(|e| panic!("load {name}: {e}"));
            cgs.validate()
                .unwrap_or_else(|e| panic!("validate {name}: {e}"));
        }
    }

    #[test]
    fn view_embed_row_resolution_uses_cached_embedded_refs() {
        let mat = RelationMaterialization::ViewEmbed {
            view: "lang_triage_context".into(),
        };
        let res = resolve_relation_row_resolution(
            &mat,
            Cardinality::Many,
            "tags",
            "LangTag",
            &crate::fixture_value!({ "item_id": "i1" }),
            None,
            |_| false,
        );
        assert_eq!(res, RelationRowResolution::EmbeddedRefs(vec![]));
    }
}
