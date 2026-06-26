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

/// Fork base snapshots for optimistic commit validation (CEP-11 / CEP-12 / CEP-14).
///
/// Graph fork-base content is captured **lazily** on first branch mutation per `Ref`
/// ([`GraphCache::fork_for_branch`]); only response/query auxiliary stores are snapshotted eagerly.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchMaterializationBase {
    pub responses: HashMap<RequestFingerprint, Arc<StoredResponse>>,
    pub query_index: HashMap<QueryCacheKey, Vec<Ref>>,
}

impl BranchMaterializationBase {
    pub fn fork_from(hot: &SessionMaterialization) -> (SessionMaterialization, Self) {
        let base = Self {
            responses: hot.responses.entries_snapshot(),
            query_index: hot.query_index.entries_snapshot(),
        };
        let branch = SessionMaterialization {
            graph: hot.graph.fork_for_branch(),
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
    let graph_write_set = branch.graph.branch_write_set();
    let graph_refs =
        GraphCache::detect_branch_write_conflicts(&session.graph, &branch.graph, &graph_write_set);

    let response_write_set = branch.responses.branch_write_fingerprints(&base.responses);
    let response_fingerprints = SessionResponseStore::detect_write_conflicts(
        &session.responses,
        &branch.responses,
        &base.responses,
        &response_write_set,
    );

    let query_write_set = branch.query_index.branch_write_keys(&base.query_index);
    let query_keys = QueryIndex::detect_write_conflicts(
        &session.query_index,
        &branch.query_index,
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
mod tests;
