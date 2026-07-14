//! Typed candidate graph with complete schema relations (internal; legend is a projection).

use std::collections::HashSet;

use indexmap::IndexMap;

use crate::discovery::outgoing_relation_hints_for_entity;
use crate::discovery_auto_seed::EntityCandidateBundle;
use crate::schema::CGS;

/// Complete outgoing relation edge from schema (not capped for internal use).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRelationEdge {
    pub wire: String,
    pub target_entity: String,
}

/// One entity node in the typed candidate graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateGraphNode {
    pub entry_id: String,
    pub entity: String,
    pub bundle: EntityCandidateBundle,
    pub outgoing: Vec<CandidateRelationEdge>,
    pub parents: Vec<String>,
}

/// Schema-backed graph for invariant normalization and closure-aware pool ops.
#[derive(Debug, Clone, Default)]
pub struct TypedCandidateGraph {
    nodes: IndexMap<(String, String), CandidateGraphNode>,
}

impl TypedCandidateGraph {
    pub fn build(
        bundles: &[EntityCandidateBundle],
        catalogs: &IndexMap<String, std::sync::Arc<CGS>>,
    ) -> Self {
        let mut nodes = IndexMap::new();
        for bundle in bundles {
            let key = (bundle.entry_id.clone(), bundle.entity.clone());
            let outgoing = catalogs
                .get(&bundle.entry_id)
                .map(|cgs| schema_outgoing(cgs.as_ref(), &bundle.entity))
                .unwrap_or_default();
            nodes.insert(
                key.clone(),
                CandidateGraphNode {
                    entry_id: bundle.entry_id.clone(),
                    entity: bundle.entity.clone(),
                    bundle: bundle.clone(),
                    outgoing,
                    parents: Vec::new(),
                },
            );
        }
        let keys: Vec<_> = nodes.keys().cloned().collect();
        for (entry_id, entity) in keys {
            if let Some(cgs) = catalogs.get(&entry_id) {
                let parents = schema_parents(cgs.as_ref(), &entity);
                if let Some(node) = nodes.get_mut(&(entry_id.clone(), entity.clone())) {
                    node.parents = parents;
                }
            }
        }
        Self { nodes }
    }

    pub fn node(&self, entry_id: &str, entity: &str) -> Option<&CandidateGraphNode> {
        self.nodes.get(&(entry_id.to_string(), entity.to_string()))
    }

    pub fn bundles(&self) -> Vec<EntityCandidateBundle> {
        self.nodes.values().map(|n| n.bundle.clone()).collect()
    }

    pub fn is_schema_relation_leaf(&self, entry_id: &str, entity: &str) -> bool {
        self.node(entry_id, entity)
            .is_some_and(|n| !n.parents.is_empty())
    }

    pub fn parent_for_leaf(&self, entry_id: &str, leaf: &str) -> Option<String> {
        self.node(entry_id, leaf)
            .and_then(|n| n.parents.first().cloned())
    }

    pub fn relation_hints_complete(
        &self,
        catalogs: &IndexMap<String, std::sync::Arc<CGS>>,
        entry_id: &str,
        entity: &str,
    ) -> String {
        if let Some(cgs) = catalogs.get(entry_id) {
            return outgoing_relation_hints_for_entity(cgs.as_ref(), entity, usize::MAX);
        }
        self.node(entry_id, entity)
            .map(|n| {
                n.outgoing
                    .iter()
                    .map(|e| format!("{}→{}", e.wire, e.target_entity))
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default()
    }

    /// Ensure parent nodes exist when a schema leaf is retained after diversification.
    pub fn closure_aware_merge(
        &self,
        diversified: Vec<EntityCandidateBundle>,
        required: &[EntityCandidateBundle],
    ) -> Vec<EntityCandidateBundle> {
        let mut out: IndexMap<(String, String), EntityCandidateBundle> = diversified
            .into_iter()
            .map(|b| ((b.entry_id.clone(), b.entity.clone()), b))
            .collect();
        for bundle in required {
            out.entry((bundle.entry_id.clone(), bundle.entity.clone()))
                .or_insert_with(|| bundle.clone());
        }
        let keys: Vec<_> = out.keys().cloned().collect();
        for (entry_id, entity) in keys {
            if !self.is_schema_relation_leaf(&entry_id, &entity) {
                continue;
            }
            if let Some(parent) = self.parent_for_leaf(&entry_id, &entity) {
                let pkey = (entry_id.clone(), parent.clone());
                if !out.contains_key(&pkey) {
                    if let Some(node) = self.node(&entry_id, &parent) {
                        out.insert(pkey, node.bundle.clone());
                    }
                }
            }
        }
        out.into_values().collect()
    }
}

fn schema_outgoing(cgs: &CGS, entity: &str) -> Vec<CandidateRelationEdge> {
    let Some(ent) = cgs.get_entity(entity) else {
        return Vec::new();
    };
    let mut edges: Vec<_> = ent
        .relations
        .iter()
        .filter_map(|(wire, rel)| {
            if cgs.get_entity(rel.target_resource.as_str()).is_some() {
                Some(CandidateRelationEdge {
                    wire: wire.to_string(),
                    target_entity: rel.target_resource.to_string(),
                })
            } else {
                None
            }
        })
        .collect();
    edges.sort_by(|a, b| a.wire.cmp(&b.wire));
    edges
}

fn schema_parents(cgs: &CGS, leaf: &str) -> Vec<String> {
    let mut parents: Vec<String> = cgs
        .entities
        .keys()
        .filter(|name| {
            cgs.get_entity(name.as_str()).is_some_and(|ent| {
                ent.relations
                    .values()
                    .any(|rel| rel.target_resource == leaf)
            })
        })
        .map(|n| n.to_string())
        .collect();
    parents.sort_unstable();
    parents
}

/// Diversification that never drops a schema parent when its leaf survives.
pub fn diversify_with_relation_closure(
    bundles: Vec<EntityCandidateBundle>,
    catalogs: &IndexMap<String, std::sync::Arc<CGS>>,
    max_entities: usize,
) -> Vec<EntityCandidateBundle> {
    let graph = TypedCandidateGraph::build(&bundles, catalogs);
    let mut sorted = bundles;
    sorted.sort_by(|a, b| {
        b.max_lexical_score
            .cmp(&a.max_lexical_score)
            .then_with(|| a.entry_id.cmp(&b.entry_id))
            .then_with(|| a.entity.cmp(&b.entity))
    });
    let kept: Vec<EntityCandidateBundle> = sorted.into_iter().take(max_entities).collect();
    let kept_keys: HashSet<_> = kept
        .iter()
        .map(|b| (b.entry_id.as_str(), b.entity.as_str()))
        .collect();
    let mut required = Vec::new();
    for (entry_id, entity) in &kept_keys {
        if graph.is_schema_relation_leaf(entry_id, entity) {
            if let Some(parent) = graph.parent_for_leaf(entry_id, entity) {
                let pkey = (entry_id.to_string(), parent);
                if !kept_keys.contains(&(pkey.0.as_str(), pkey.1.as_str())) {
                    if let Some(node) = graph.node(entry_id, &pkey.1) {
                        required.push(node.bundle.clone());
                    }
                }
            }
        }
    }
    graph.closure_aware_merge(kept, &required)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::loader::load_schema_dir;
    use std::path::PathBuf;

    fn matrix_graph() -> (IndexMap<String, Arc<CGS>>, TypedCandidateGraph) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_prompt_matrix");
        let cgs = Arc::new(load_schema_dir(&dir).expect("matrix"));
        let mut catalogs = IndexMap::new();
        catalogs.insert("prompt_matrix".into(), cgs);
        let bundles = vec![];
        let graph = TypedCandidateGraph::build(&bundles, &catalogs);
        (catalogs, graph)
    }

    #[test]
    fn graph_builds_from_empty_bundles() {
        let (_catalogs, graph) = matrix_graph();
        assert!(graph.bundles().is_empty());
    }
}
