//! Per-store optimistic branch commit validation (graph + response cache + query index).

use std::collections::HashMap;
use std::sync::Arc;

use plasm_core::Ref;

use crate::cache::GraphCache;
use crate::materialization::{SessionMaterialization, SessionResponseStore, StoredResponse};
use crate::query_index::{QueryCacheKey, QueryIndex};
use crate::replay::RequestFingerprint;

/// Conflicts detected when merging a branch back into the session hot materialization.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WriteConflictDetails {
    pub graph_refs: Vec<Ref>,
    pub response_fingerprints: Vec<RequestFingerprint>,
    pub query_keys: Vec<QueryCacheKey>,
}

impl WriteConflictDetails {
    pub fn has_any(&self) -> bool {
        !self.graph_refs.is_empty()
            || !self.response_fingerprints.is_empty()
            || !self.query_keys.is_empty()
    }
}

/// Fork base snapshots for optimistic commit validation (CEP-11 / CEP-12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchMaterializationBase {
    pub graph_versions: HashMap<Ref, u64>,
    pub responses: HashMap<RequestFingerprint, Arc<StoredResponse>>,
    pub query_index: HashMap<QueryCacheKey, Vec<Ref>>,
}

impl BranchMaterializationBase {
    pub fn fork_from(hot: &SessionMaterialization) -> (SessionMaterialization, Self) {
        let (graph, graph_versions) = hot.graph.clone_capturing_ref_versions();
        let base = Self {
            graph_versions,
            responses: hot.responses.entries_snapshot(),
            query_index: hot.query_index.entries_snapshot(),
        };
        let branch = SessionMaterialization {
            graph,
            responses: hot.responses.clone(),
            query_index: hot.query_index.clone(),
        };
        (branch, base)
    }
}

pub fn detect_materialization_conflicts(
    session: &SessionMaterialization,
    base: &BranchMaterializationBase,
    branch: &SessionMaterialization,
) -> WriteConflictDetails {
    let graph_write_set = branch.graph.branch_write_set(&base.graph_versions);
    let graph_refs =
        GraphCache::detect_write_conflicts(&session.graph, &base.graph_versions, &graph_write_set);

    let response_write_set = branch.responses.branch_write_fingerprints(&base.responses);
    let response_fingerprints = SessionResponseStore::detect_write_conflicts(
        &session.responses,
        &base.responses,
        &response_write_set,
    );

    let query_write_set = branch.query_index.branch_write_keys(&base.query_index);
    let query_keys = QueryIndex::detect_write_conflicts(
        &session.query_index,
        &base.query_index,
        &query_write_set,
    );

    WriteConflictDetails {
        graph_refs,
        response_fingerprints,
        query_keys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExecutionSource;

    #[test]
    fn response_store_write_conflict_on_concurrent_insert() {
        let mut session = SessionResponseStore::default();
        let fp = RequestFingerprint::from_hex(&format!("{:064x}", 7u64)).expect("fp");
        session.store(
            fp.clone(),
            serde_json::json!({"a": 1}),
            ExecutionSource::Live,
        );

        let base = session.entries_snapshot();
        let mut branch = session.clone();
        branch.store(
            fp.clone(),
            serde_json::json!({"a": 2}),
            ExecutionSource::Live,
        );
        let write_set = branch.branch_write_fingerprints(&base);
        assert!(
            SessionResponseStore::detect_write_conflicts(&session, &base, &write_set).is_empty()
        );

        session.store(
            fp.clone(),
            serde_json::json!({"a": 9}),
            ExecutionSource::Live,
        );
        let conflicts = SessionResponseStore::detect_write_conflicts(&session, &base, &write_set);
        assert_eq!(conflicts, vec![fp]);
    }

    #[test]
    fn query_index_write_conflict_on_concurrent_key() {
        let mut session = QueryIndex::default();
        let key = QueryCacheKey::test("scoped\0label=1");
        let r1 = Ref::new("Label", "1");
        session.insert(key.clone(), vec![r1.clone()]);

        let base = session.entries_snapshot();
        let mut branch = session.clone();
        branch.insert(key.clone(), vec![Ref::new("Label", "2")]);
        let write_set = branch.branch_write_keys(&base);
        assert!(QueryIndex::detect_write_conflicts(&session, &base, &write_set).is_empty());

        session.insert(key.clone(), vec![Ref::new("Label", "9")]);
        let conflicts = QueryIndex::detect_write_conflicts(&session, &base, &write_set);
        assert_eq!(conflicts, vec![key]);
    }
}
