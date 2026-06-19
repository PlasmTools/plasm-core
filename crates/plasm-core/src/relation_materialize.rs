//! Typed per-parent relation row resolution (CGS `materialize` → embed vs scoped HTTP).
//!
//! Plan and runtime share this module so strategy does not depend on cache-shape heuristics.
//!
//! **Typed row invariant:** relation traversals must yield rows shaped as the target CGS entity
//! (full field set or explicit nulls). Wire embeds are transport shortcuts; incomplete embeds
//! are hydrated via target GET before plan compute (see `plasm_plan_run::relation_hydrate`).

use crate::{Cardinality, EmbedOnMissPolicy, JsonPathSegment, Ref, RelationMaterialization};
use serde_json::Value;

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
    relation_name: &str,
    expected_target: &str,
    parent_json: &Value,
    relation_refs: Option<&[Ref]>,
    graph_has_ref: impl Fn(&Ref) -> bool,
) -> RelationRowResolution {
    match materialize {
        RelationMaterialization::Unavailable => RelationRowResolution::ScopedQuery,
        RelationMaterialization::FromParentGet { .. } => RelationRowResolution::EmbeddedRefs(
            relation_refs.map(|s| s.to_vec()).unwrap_or_default(),
        ),
        RelationMaterialization::QueryScoped { .. }
        | RelationMaterialization::QueryScopedBindings { .. } => RelationRowResolution::ScopedQuery,
        RelationMaterialization::GetScopedBindings { .. } => RelationRowResolution::ScopedQuery,
        RelationMaterialization::PreferFromParentGet {
            path,
            on_embed_miss,
            fallback: _,
        } => resolve_prefer_from_parent_get_row(
            path,
            *on_embed_miss,
            relation_name,
            expected_target,
            parent_json,
            relation_refs,
            graph_has_ref,
        ),
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
    let extracted = extract_from_parent_get_value(parent_json, path);
    if extracted.iter().all(|v| v.is_null()) || extracted.is_empty() {
        return RelationRowResolution::ScopedQuery;
    }
    let Some(refs) = relation_refs else {
        return RelationRowResolution::ScopedQuery;
    };
    if refs.is_empty() {
        return RelationRowResolution::ScopedQuery;
    }
    if relation_refs_fully_resolved(refs, expected_target, |r| graph_has_ref(r).then_some(&())) {
        return RelationRowResolution::EmbeddedRefs(refs.to_vec());
    }
    match on_embed_miss {
        EmbedOnMissPolicy::FallbackScoped => RelationRowResolution::ScopedQuery,
    }
}

/// Per-parent embed vs scoped resolution for plan/runtime prefer materialization.
pub fn partition_prefer_resolutions<'a, F>(
    materialize: &RelationMaterialization,
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
/// pairs are allowed when runtime decode is single-hop (CEP-10).
pub fn validate_from_parent_get_embed_acyclic(cgs: &crate::CGS) -> Result<(), String> {
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
            return Err(cycle.join(" → "));
        }
    }
    Ok(())
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
    source_rows: &[Value],
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
    fn pure_query_scoped_always_scoped() {
        let mat = RelationMaterialization::QueryScopedBindings {
            capability: "cap".into(),
            bindings: IndexMap::new(),
        };
        let res = resolve_relation_row_resolution(
            &mat,
            "tags",
            "Tag",
            &serde_json::json!({"tags": [{"id": 1}]}),
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
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::QueryScopedBindings {
                capability: "issue_label_query".into(),
                bindings: IndexMap::new(),
            },
        };
        let res = resolve_relation_row_resolution(
            &mat,
            "labels",
            "Label",
            &serde_json::json!({"labels": []}),
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
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::QueryScopedBindings {
                capability: "issue_label_query".into(),
                bindings: IndexMap::new(),
            },
        };
        let refs = vec![Ref::new("Label", "99")];
        let row = serde_json::json!({"labels": [{"id": 99}]});
        let resolutions = partition_prefer_resolutions(
            &mat,
            "labels",
            "Label",
            [(&row, Some(refs.as_slice()))],
            |_| false,
        );
        assert_eq!(resolutions.len(), 1);
        assert_eq!(resolutions[0], RelationRowResolution::ScopedQuery);
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
            on_embed_miss: EmbedOnMissPolicy::FallbackScoped,
            fallback: RelationScopedFallback::HydrateFromEmbedPath {
                path: path.clone(),
                get_capability: "pokemon_get".into(),
            },
        };
        let row = serde_json::json!({
            "name": "electric",
            "pokemon": [
                { "pokemon": { "name": "pikachu", "url": "https://pokeapi.co/api/v2/pokemon/25/" } }
            ]
        });
        let extracted = extract_from_parent_get_value(&row, &path);
        assert_eq!(extracted.len(), 1);
        assert_eq!(extracted[0]["name"], "pikachu");
        let res = resolve_relation_row_resolution(
            &mat,
            "pokemon",
            "Pokemon",
            &row,
            None,
            |_| false,
        );
        assert_eq!(res, RelationRowResolution::ScopedQuery);
    }

    #[test]
    fn pokeapi_mutual_prefer_embed_loads_and_validates() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        let cgs = crate::loader::load_schema(&dir).expect("pokeapi");
        cgs.validate().expect("pokeapi validates with mutual prefer embeds");
        validate_from_parent_get_embed_acyclic(&cgs).expect("forward from_parent_get edges acyclic");
        let type_rel = cgs
            .get_entity("Type")
            .and_then(|e| e.relations.get("pokemon"))
            .expect("Type.pokemon");
        assert!(matches!(
            type_rel.materialize,
            Some(RelationMaterialization::PreferFromParentGet { .. })
        ));
    }

    #[test]
    fn all_packaged_api_catalogs_load_and_validate() {
        let apis_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis");
        let mut count = 0usize;
        for entry in std::fs::read_dir(&apis_root).expect("apis dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if !path.is_dir() || !path.join("domain.yaml").is_file() {
                continue;
            }
            let cgs = crate::loader::load_schema(&path)
                .unwrap_or_else(|e| panic!("load {}: {e}", path.display()));
            cgs.validate()
                .unwrap_or_else(|e| panic!("validate {}: {e}", path.display()));
            count += 1;
        }
        assert!(count > 5, "expected multiple API catalogs under apis/");
    }
}
