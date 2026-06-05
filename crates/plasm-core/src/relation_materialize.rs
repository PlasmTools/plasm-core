//! Typed per-parent relation row resolution (CGS `materialize` → embed vs scoped HTTP).
//!
//! Plan and runtime share this module so strategy does not depend on cache-shape heuristics.

use crate::{
    Cardinality, EmbedOnMissPolicy, JsonPathSegment, Ref, RelationMaterialization,
    RelationScopedFallback,
};
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
    refs.iter().all(|r| {
        r.entity_type.as_str() == expected_target && get(r).is_some()
    })
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
    relation_name: &str,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RelationMaterialization;
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
        let res = resolve_relation_row_resolution(
            &mat,
            "labels",
            "Label",
            &serde_json::json!({"labels": [{"id": 99}]}),
            Some(&refs),
            |_| false,
        );
        assert_eq!(res, RelationRowResolution::ScopedQuery);
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
