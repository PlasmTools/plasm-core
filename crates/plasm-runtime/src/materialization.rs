//! Session materialization facade: entity graph + response fingerprints + query index.

use crate::cache::{CachedEntity, EntityCompleteness, GraphCache};
use crate::query_index::{QueryCacheKey, QueryIndex};
use crate::replay::{MemoryReplayStore, ReplayEntry, ReplayStore, RequestFingerprint};
use crate::{ExecutionSource, RuntimeError};
use plasm_core::{CompOp, GetExpr, QueryExpr, Ref, CGS};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Honest cache consult counters (distinct from output row counts).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheTelemetry {
    pub entity_graph_hits: usize,
    pub entity_graph_misses: usize,
    pub response_store_hits: usize,
    pub response_store_misses: usize,
    pub query_satisfied_from_graph: usize,
    pub query_required_network: usize,
    pub rows_materialized: usize,
}

impl CacheTelemetry {
    pub fn merge(&mut self, other: &Self) {
        self.entity_graph_hits = self
            .entity_graph_hits
            .saturating_add(other.entity_graph_hits);
        self.entity_graph_misses = self
            .entity_graph_misses
            .saturating_add(other.entity_graph_misses);
        self.response_store_hits = self
            .response_store_hits
            .saturating_add(other.response_store_hits);
        self.response_store_misses = self
            .response_store_misses
            .saturating_add(other.response_store_misses);
        self.query_satisfied_from_graph = self
            .query_satisfied_from_graph
            .saturating_add(other.query_satisfied_from_graph);
        self.query_required_network = self
            .query_required_network
            .saturating_add(other.query_required_network);
        self.rows_materialized = self
            .rows_materialized
            .saturating_add(other.rows_materialized);
    }

    /// Legacy trace fields: hits = consult hits; misses = consult misses (not row count).
    pub fn legacy_cache_hits(&self) -> usize {
        self.entity_graph_hits
            .saturating_add(self.response_store_hits)
            .saturating_add(self.query_satisfied_from_graph)
    }

    pub fn legacy_cache_misses(&self) -> usize {
        self.entity_graph_misses
            .saturating_add(self.response_store_misses)
            .saturating_add(self.query_required_network)
    }
}

/// Read-only entity graph snapshot for parallel fanout consult.
#[derive(Debug, Clone)]
pub struct EntityGraphSnapshot {
    inner: GraphCache,
}

impl EntityGraphSnapshot {
    pub fn from_graph(graph: &GraphCache) -> Self {
        Self {
            inner: graph.clone(),
        }
    }

    pub fn get(&self, reference: &Ref) -> Option<&CachedEntity> {
        self.inner.get(reference)
    }

    pub fn get_entities_by_type(&self, entity_type: &str) -> Vec<CachedEntity> {
        self.inner
            .get_entities_by_type(entity_type)
            .into_iter()
            .cloned()
            .collect()
    }

    pub fn into_graph(self) -> GraphCache {
        self.inner
    }
}

/// Live-session response cache (same fingerprint semantics as replay).
#[derive(Debug, Clone, Default)]
pub struct SessionResponseStore {
    entries: std::collections::HashMap<RequestFingerprint, Arc<StoredResponse>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResponse {
    pub response: serde_json::Value,
    pub source: ExecutionSource,
}

impl SessionResponseStore {
    pub fn lookup(&self, fingerprint: &RequestFingerprint) -> Option<Arc<StoredResponse>> {
        self.entries.get(fingerprint).cloned()
    }

    pub fn store(
        &mut self,
        fingerprint: RequestFingerprint,
        response: serde_json::Value,
        source: ExecutionSource,
    ) {
        self.entries
            .insert(fingerprint, Arc::new(StoredResponse { response, source }));
    }

    pub fn invalidate_entity_type(&mut self, _entity_type: &str) {
        // Conservative: scoped queries may return stale rows after mutation; clear all.
        self.entries.clear();
    }

    pub fn merge_from(&mut self, other: SessionResponseStore) {
        self.entries.extend(other.entries);
    }

    pub(crate) fn entries_snapshot(
        &self,
    ) -> std::collections::HashMap<RequestFingerprint, Arc<StoredResponse>> {
        self.entries.clone()
    }

    pub(crate) fn branch_write_fingerprints(
        &self,
        base: &std::collections::HashMap<RequestFingerprint, Arc<StoredResponse>>,
    ) -> Vec<RequestFingerprint> {
        self.entries
            .iter()
            .filter_map(|(fp, arc)| match base.get(fp) {
                None => Some(fp.clone()),
                Some(base_arc) if !Arc::ptr_eq(arc, base_arc) => Some(fp.clone()),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn detect_write_conflicts(
        session: &Self,
        base: &std::collections::HashMap<RequestFingerprint, Arc<StoredResponse>>,
        write_set: &[RequestFingerprint],
    ) -> Vec<RequestFingerprint> {
        write_set
            .iter()
            .filter(|fp| match base.get(*fp) {
                None => session.entries.contains_key(*fp),
                Some(base_arc) => session
                    .entries
                    .get(*fp)
                    .is_none_or(|live| !Arc::ptr_eq(live, base_arc)),
            })
            .cloned()
            .collect()
    }
}

/// Unified per-session materialization state.
#[derive(Debug, Clone, Default)]
pub struct SessionMaterialization {
    pub graph: GraphCache,
    pub responses: SessionResponseStore,
    pub query_index: QueryIndex,
}

impl SessionMaterialization {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn graph_mut(&mut self) -> &mut GraphCache {
        &mut self.graph
    }

    pub fn snapshot(&self) -> EntityGraphSnapshot {
        EntityGraphSnapshot::from_graph(&self.graph)
    }

    pub fn merge_graph(&mut self, entities: Vec<CachedEntity>) -> Result<usize, RuntimeError> {
        self.graph.merge(entities)
    }

    pub fn invalidate_after_mutation(&mut self, entity_type: &str) {
        self.query_index.invalidate_entity_type(entity_type);
        self.responses.invalidate_entity_type(entity_type);
    }

    /// Merge fanout branch materialization back into the session (graph + response + query index).
    pub fn absorb_branch(&mut self, branch: SessionMaterialization) -> Result<usize, RuntimeError> {
        let merged = self.graph.merge_from_graph(&branch.graph)?;
        self.responses.merge_from(branch.responses);
        self.query_index.merge_from(branch.query_index);
        Ok(merged)
    }
}

impl std::ops::Deref for SessionMaterialization {
    type Target = GraphCache;
    fn deref(&self) -> &Self::Target {
        &self.graph
    }
}

impl std::ops::DerefMut for SessionMaterialization {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.graph
    }
}

/// Where plan compute reads rows after surface materialization.
#[derive(Debug, Clone)]
pub enum MaterializedRowSource {
    /// Rows fully in memory (small / non-graph-backed).
    Inline(Vec<serde_json::Value>),
    /// Hot graph + spilled pages; `logical_count` from [`crate::ExecutionResult::count`].
    ///
    /// `hot_snapshot` is captured at materialize time so compute can rehydrate without
    /// re-acquiring the session graph mutex or re-copying hot entities.
    GraphBacked {
        entity_type: String,
        logical_count: usize,
        hot_snapshot: std::sync::Arc<[CachedEntity]>,
    },
}

impl MaterializedRowSource {
    pub fn inline_rows(&self) -> Option<&[serde_json::Value]> {
        match self {
            Self::Inline(rows) => Some(rows.as_slice()),
            Self::GraphBacked { .. } => None,
        }
    }

    pub fn is_graph_backed(&self) -> bool {
        matches!(self, Self::GraphBacked { .. })
    }
}

/// Outcome of a cache consult before HTTP.
#[derive(Debug, Clone)]
pub enum CacheDecision {
    SatisfiedFromGraph {
        entities: Vec<CachedEntity>,
    },
    SatisfiedFromResponse {
        response: serde_json::Value,
        source: ExecutionSource,
    },
    RequiresNetwork,
}

pub struct ExecutionCacheConsult;

impl ExecutionCacheConsult {
    pub fn decide_get(
        get: &GetExpr,
        snapshot: &EntityGraphSnapshot,
        telemetry: &mut CacheTelemetry,
    ) -> Option<CachedEntity> {
        let entity = snapshot.get(&get.reference)?;
        if entity.completeness == EntityCompleteness::Complete {
            telemetry.entity_graph_hits += 1;
            Some(entity.clone())
        } else {
            None
        }
    }

    pub fn record_get_miss(telemetry: &mut CacheTelemetry) {
        telemetry.entity_graph_misses += 1;
    }

    pub fn decide_response(
        fingerprint: &RequestFingerprint,
        responses: &SessionResponseStore,
        telemetry: &mut CacheTelemetry,
    ) -> Option<StoredResponse> {
        let stored = responses.lookup(fingerprint)?;
        telemetry.response_store_hits += 1;
        Some((*stored).clone())
    }

    pub fn record_response_miss(telemetry: &mut CacheTelemetry) {
        telemetry.response_store_misses += 1;
    }

    pub fn decide_query(
        query: &QueryExpr,
        capability_name: &str,
        snapshot: &EntityGraphSnapshot,
        query_index: &QueryIndex,
        cgs: &CGS,
        telemetry: &mut CacheTelemetry,
    ) -> Option<Vec<CachedEntity>> {
        let key = QueryCacheKey::from_query(query, capability_name)?;
        let refs = query_index.get(&key)?;
        if refs.is_empty() {
            return None;
        }
        let mut entities = Vec::with_capacity(refs.len());
        for r in refs {
            let e = snapshot.get(r)?;
            if query.predicate.as_ref().is_some_and(|p| {
                cgs.get_entity(&query.entity)
                    .is_some_and(|def| !client_side_predicate_matches_entity(e, p, def))
            }) {
                return None;
            }
            entities.push(e.clone());
        }
        telemetry.query_satisfied_from_graph += 1;
        Some(entities)
    }

    pub fn record_query_network(telemetry: &mut CacheTelemetry) {
        telemetry.query_required_network += 1;
    }

    pub fn index_query_result(
        mat: &mut SessionMaterialization,
        query: &QueryExpr,
        capability_name: &str,
        entities: &[CachedEntity],
    ) {
        if let Some(key) = QueryCacheKey::from_query(query, capability_name) {
            let refs: Vec<Ref> = entities.iter().map(|e| e.reference.clone()).collect();
            mat.query_index.insert(key, refs);
        }
    }
}

#[allow(clippy::only_used_in_recursion)]
fn client_side_predicate_matches_entity(
    entity: &CachedEntity,
    pred: &plasm_core::Predicate,
    entity_def: &plasm_core::EntityDef,
) -> bool {
    use plasm_core::Predicate;
    match pred {
        Predicate::Comparison {
            field,
            op: CompOp::Eq,
            value,
        } => entity
            .get_field(field)
            .map(|f| f.to_value() == value.to_value())
            .unwrap_or(false),
        Predicate::And { args } => args
            .iter()
            .all(|c| client_side_predicate_matches_entity(entity, c, entity_def)),
        _ => false,
    }
}

/// Collect fanout branch deltas and merge into session materialization (single writer).
pub struct FanoutCoordinator<'a> {
    mat: &'a mut SessionMaterialization,
}

impl<'a> FanoutCoordinator<'a> {
    pub fn new(mat: &'a mut SessionMaterialization) -> Self {
        Self { mat }
    }

    pub fn reader(&self) -> EntityGraphSnapshot {
        self.mat.snapshot()
    }

    pub fn responses(&self) -> &SessionResponseStore {
        &self.mat.responses
    }

    pub fn merge_entities(&mut self, entities: Vec<CachedEntity>) -> Result<usize, RuntimeError> {
        self.mat.merge_graph(entities)
    }

    pub fn mat_mut(&mut self) -> &mut SessionMaterialization {
        self.mat
    }
}

/// Bridge for tests that still use [`MemoryReplayStore`].
pub fn replay_store_lookup(
    store: &MemoryReplayStore,
    fingerprint: &RequestFingerprint,
) -> Result<Option<ReplayEntry>, RuntimeError> {
    store.lookup(fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::ExecutionStats;
    use plasm_core::{EntityName, Predicate, QueryExpr, Value};

    #[test]
    fn cache_telemetry_legacy_hits_aggregate_consult_counters() {
        let t = CacheTelemetry {
            entity_graph_hits: 1,
            response_store_hits: 2,
            query_satisfied_from_graph: 3,
            entity_graph_misses: 4,
            response_store_misses: 5,
            query_required_network: 6,
            rows_materialized: 9,
        };
        assert_eq!(t.legacy_cache_hits(), 6);
        assert_eq!(t.legacy_cache_misses(), 15);
        assert_eq!(t.rows_materialized, 9);
    }

    #[test]
    fn session_response_store_reuses_fingerprint() {
        let mut store = SessionResponseStore::default();
        let fp = RequestFingerprint::from_hex(
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
        )
        .expect("fp");
        store.store(
            fp.clone(),
            serde_json::json!({"ok": true}),
            ExecutionSource::Live,
        );
        assert!(store.lookup(&fp).is_some());
    }

    #[test]
    fn query_consult_serves_indexed_scoped_query() {
        let mut mat = SessionMaterialization::new();
        let mut q = QueryExpr::filtered(EntityName::new("Label"), Predicate::eq("owner", "o"));
        q.capability_name = Some(plasm_core::CapabilityName::new("issue_label_query"));
        let mut fields = indexmap::IndexMap::new();
        fields.insert("owner".into(), Value::String("o".into()));
        let entity = CachedEntity::from_decoded(
            Ref::new("Label", "1"),
            fields,
            indexmap::IndexMap::new(),
            0,
            EntityCompleteness::Complete,
        );
        mat.insert(entity.clone()).expect("insert");
        ExecutionCacheConsult::index_query_result(&mut mat, &q, "issue_label_query", &[entity]);
        let snapshot = mat.snapshot();
        let mut telemetry = CacheTelemetry::default();
        let served = ExecutionCacheConsult::decide_query(
            &q,
            "issue_label_query",
            &snapshot,
            &mat.query_index,
            &CGS::new(),
            &mut telemetry,
        )
        .expect("indexed query should consult-hit");
        assert_eq!(served.len(), 1);
        assert_eq!(telemetry.query_satisfied_from_graph, 1);
        assert_eq!(telemetry.query_required_network, 0);
    }

    #[test]
    fn execution_stats_rows_materialized_not_consult_miss() {
        let stats = ExecutionStats::from_telemetry(
            CacheTelemetry {
                rows_materialized: 9,
                query_required_network: 1,
                ..Default::default()
            },
            1,
        );
        assert_eq!(stats.cache.rows_materialized, 9);
        assert_eq!(stats.cache_misses, 1);
        assert_ne!(stats.cache_misses, stats.cache.rows_materialized);
    }
}
