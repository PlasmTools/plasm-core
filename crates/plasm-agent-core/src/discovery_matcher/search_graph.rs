//! Directed catalogue relationships. Creation is a nomination, never an output binding.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Relationship {
    SuppliesInput,
    RequiredAcquisition,
    MayCreateResource,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct Edge<K> {
    pub source: K,
    pub consumer: K,
    pub relationship: Relationship,
    /// Identity in the pinned catalogue projection; not model-generated prose.
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GraphError<K> {
    Unauthorized(K),
}

pub(super) struct SearchGraph<K: Ord> {
    authorized: BTreeSet<K>,
    incoming: BTreeMap<K, BTreeSet<Edge<K>>>,
}

impl<K: Ord + Clone> SearchGraph<K> {
    pub fn new(
        authorized: BTreeSet<K>,
        edges: impl IntoIterator<Item = Edge<K>>,
    ) -> Result<Self, GraphError<K>> {
        let mut incoming = BTreeMap::<K, BTreeSet<Edge<K>>>::new();
        for edge in edges {
            for key in [&edge.source, &edge.consumer] {
                if !authorized.contains(key) {
                    return Err(GraphError::Unauthorized(key.clone()));
                }
            }
            incoming
                .entry(edge.consumer.clone())
                .or_default()
                .insert(edge);
        }
        Ok(Self {
            authorized,
            incoming,
        })
    }

    /// Nominate only one hop from retained capabilities. An optional supplier or
    /// possible creator does not unlock its own neighbours until retained.
    pub fn nominate(&self, retained: &BTreeSet<K>) -> BTreeSet<Edge<K>> {
        retained
            .iter()
            .flat_map(|key| self.incoming.get(key).into_iter().flatten())
            .filter(|edge| {
                edge.relationship != Relationship::RequiredAcquisition
                    && !retained.contains(&edge.source)
            })
            .cloned()
            .collect()
    }

    pub fn authorized(&self) -> &BTreeSet<K> {
        &self.authorized
    }

    pub fn edges(&self) -> impl Iterator<Item = &Edge<K>> {
        self.incoming.values().flatten()
    }

    pub fn incoming(&self, consumer: &K) -> impl Iterator<Item = &Edge<K>> {
        self.incoming.get(consumer).into_iter().flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(source: usize, consumer: usize, relationship: Relationship) -> Edge<usize> {
        Edge {
            source,
            consumer,
            relationship,
            provenance: format!("catalogue:{source}:{consumer}"),
        }
    }

    #[test]
    fn incoming_index_matches_complete_edge_filter() {
        let graph = SearchGraph::new(
            (0..4).collect(),
            [
                edge(1, 0, Relationship::SuppliesInput),
                edge(2, 0, Relationship::MayCreateResource),
                edge(0, 1, Relationship::RequiredAcquisition),
            ],
        )
        .unwrap();
        for consumer in 0..4 {
            assert_eq!(
                graph.incoming(&consumer).collect::<Vec<_>>(),
                graph
                    .edges()
                    .filter(|edge| edge.consumer == consumer)
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn absent_lexical_supplier_then_creation_requires_retention_at_each_hop() {
        let graph = SearchGraph::new(
            (0..4).collect(),
            [
                edge(1, 0, Relationship::SuppliesInput),
                edge(2, 1, Relationship::MayCreateResource),
                edge(3, 2, Relationship::RequiredAcquisition),
            ],
        )
        .unwrap();
        assert_eq!(
            graph.nominate(&BTreeSet::from([0])),
            BTreeSet::from([edge(1, 0, Relationship::SuppliesInput)])
        );
        assert_eq!(
            graph.nominate(&BTreeSet::from([0, 1])),
            BTreeSet::from([edge(2, 1, Relationship::MayCreateResource)])
        );
        assert!(graph.nominate(&BTreeSet::from([2])).is_empty());
    }

    #[test]
    fn unauthorized_edges_fail_before_dispatch() {
        assert!(matches!(
            SearchGraph::new(
                BTreeSet::from([0]),
                [edge(1, 0, Relationship::SuppliesInput)]
            ),
            Err(GraphError::Unauthorized(1))
        ));
    }

    #[test]
    fn multiple_suppliers_remain_candidates_and_optional_cycles_do_not_expand() {
        let edges = [
            edge(1, 0, Relationship::SuppliesInput),
            edge(2, 0, Relationship::SuppliesInput),
            edge(0, 1, Relationship::MayCreateResource),
        ];
        let graph = SearchGraph::new((0..3).collect(), edges.clone()).unwrap();
        let reversed = SearchGraph::new((0..3).collect(), edges.into_iter().rev()).unwrap();
        assert_eq!(
            graph.nominate(&BTreeSet::from([0])),
            reversed.nominate(&BTreeSet::from([0]))
        );
        assert_eq!(graph.nominate(&BTreeSet::from([0])).len(), 2);
    }
}
