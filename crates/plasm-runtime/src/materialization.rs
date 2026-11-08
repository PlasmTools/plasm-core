//! Session materialization facade: entity graph + response fingerprints + query index.

use crate::cache::{CachedEntity, EntityCompleteness, GraphCache};
use crate::query_index::{QueryCacheKey, QueryIndex};
use crate::replay::{MemoryReplayStore, ReplayEntry, ReplayStore, RequestFingerprint};
use crate::{ExecutionSource, RuntimeError};
use indexmap::IndexMap;
use plasm_core::prerequisites::DeploymentBindings;
use plasm_core::{CompOp, GetExpr, QueryExpr, Ref, Value, CGS};
use serde::{Deserialize, Serialize};

/// Conservative charge for hash-table buckets, including control bytes and
/// growth slack. Saturation makes oversized capacities fail the budget check.
pub(crate) fn retained_hash_capacity<K, V>(capacity: usize) -> usize {
    capacity
        .saturating_mul(std::mem::size_of::<(K, V)>().saturating_add(2))
        .saturating_mul(2)
}

pub(crate) fn retained_index_capacity<K, V>(capacity: usize) -> usize {
    capacity
        .saturating_mul(std::mem::size_of::<(K, V)>().saturating_add(16))
        .saturating_mul(2)
}

fn value_map_capacity(values: &IndexMap<String, Value>, depth: usize) -> usize {
    values.iter().fold(
        retained_index_capacity::<String, Value>(values.capacity()),
        |total, (key, value)| {
            total
                .saturating_add(key.capacity())
                .saturating_add(value_retention_capacity(value, depth))
        },
    )
}

pub(crate) fn value_retention_capacity(value: &Value, depth: usize) -> usize {
    if depth >= 64 {
        return usize::MAX;
    }
    match value {
        Value::String(s) | Value::PhraseIdent(s) => s.capacity(),
        Value::Array(values) => values.iter().fold(
            values
                .capacity()
                .saturating_mul(std::mem::size_of::<Value>()),
            |n, value| n.saturating_add(value_retention_capacity(value, depth + 1)),
        ),
        Value::Object(values) => value_map_capacity(values, depth + 1),
        Value::UnionCtor {
            ctor_label,
            ctor_fields,
        } => ctor_label
            .capacity()
            .saturating_add(value_map_capacity(ctor_fields, depth + 1)),
        // These operands can retain private compiler state not represented by
        // their serialized view, and should have been resolved before execution.
        Value::StringTemplate(_) | Value::GetScalarExtract(_) | Value::PlasmInputRef(_) => {
            usize::MAX
        }
        Value::Null
        | Value::Bool(_)
        | Value::Integer(_)
        | Value::Unsigned(_)
        | Value::Float(_)
        | Value::Money(_) => 0,
    }
}

pub(crate) fn typed_value_retention_capacity(
    value: &plasm_core::TypedFieldValue,
    depth: usize,
) -> usize {
    use plasm_core::TypedFieldValue as T;
    if depth >= 64 {
        return usize::MAX;
    }
    match value {
        T::String(s) => s.capacity(),
        T::Array(values) => values.iter().fold(
            values.capacity().saturating_mul(std::mem::size_of::<T>()),
            |n, value| n.saturating_add(typed_value_retention_capacity(value, depth + 1)),
        ),
        T::Object(values) => values.iter().fold(
            retained_index_capacity::<String, T>(values.capacity()),
            |n, (key, value)| {
                n.saturating_add(key.capacity())
                    .saturating_add(typed_value_retention_capacity(value, depth + 1))
            },
        ),
        T::Json(value) => value_retention_capacity(value, depth + 1),
        T::EntityRef(value) => entity_ref_retention_capacity(value, depth + 1),
        // These forms contain opaque compiler state and should have been resolved.
        T::StringTemplate(_) | T::PlasmInputRef(_) => usize::MAX,
        T::Null | T::Bool(_) | T::Integer(_) | T::Float(_) | T::Money(_) => 0,
    }
}

fn entity_ref_retention_capacity(
    value: &plasm_core::entity_ref_value::EntityRefPayload,
    depth: usize,
) -> usize {
    use plasm_core::entity_ref_value::{EntityRefAtom, EntityRefPayload};
    if depth >= 64 {
        return usize::MAX;
    }
    match value {
        EntityRefPayload::Atom(EntityRefAtom::String(s)) => s.capacity(),
        EntityRefPayload::Atom(_) => 0,
        EntityRefPayload::Compound(values) => values.iter().fold(
            retained_index_capacity::<String, EntityRefPayload>(values.capacity()),
            |n, (key, value)| {
                n.saturating_add(key.capacity())
                    .saturating_add(entity_ref_retention_capacity(value, depth + 1))
            },
        ),
    }
}
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

pub(crate) struct RetainedResponses<'a>(
    pub(crate) &'a std::collections::HashMap<RequestFingerprint, Arc<StoredResponse>>,
);

// Encoded size alone misses spare capacity, including after a provider body is
// shortened. Bound the walk itself as well as the continuation it measures.
pub(crate) fn json_retention_capacity(value: &serde_json::Value, depth: usize) -> usize {
    if depth == 128 {
        return usize::MAX;
    }
    match value {
        serde_json::Value::String(s) => s.capacity(),
        serde_json::Value::Array(values) => values.iter().fold(
            values
                .capacity()
                .saturating_mul(std::mem::size_of::<serde_json::Value>()),
            |total, value| total.saturating_add(json_retention_capacity(value, depth + 1)),
        ),
        serde_json::Value::Object(values) => {
            values
                .iter()
                .fold(values.len().saturating_mul(1024), |total, (key, value)| {
                    total
                        .saturating_add(key.capacity())
                        .saturating_add(json_retention_capacity(value, depth + 1))
                })
        }
        _ => 0,
    }
}

pub(crate) fn response_retention_capacity(
    entries: &std::collections::HashMap<RequestFingerprint, Arc<StoredResponse>>,
) -> usize {
    entries.values().fold(
        retained_hash_capacity::<RequestFingerprint, Arc<StoredResponse>>(entries.capacity()),
        |total, response| {
            total
                .saturating_add(json_retention_capacity(&response.outcome.response, 0))
                .saturating_add(
                    response
                        .outcome
                        .next_link
                        .as_ref()
                        .map_or(0, String::capacity),
                )
        },
    )
}
impl serde::Serialize for RetainedResponses<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for (fingerprint, response) in self.0 {
            sequence.serialize_element(&(fingerprint, &response.outcome, response.source))?;
        }
        sequence.end()
    }
}

struct RetainedPairs<'a, K, V>(&'a std::collections::HashMap<K, V>);
impl<K: serde::Serialize, V: serde::Serialize> serde::Serialize for RetainedPairs<'_, K, V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for item in self.0 {
            sequence.serialize_element(&item)?;
        }
        sequence.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredResponse {
    pub outcome: crate::replay::TransportOutcome,
    pub source: ExecutionSource,
}

impl SessionResponseStore {
    pub(crate) fn retention_capacity_bytes(&self) -> usize {
        response_retention_capacity(&self.entries)
    }
    pub fn lookup(&self, fingerprint: &RequestFingerprint) -> Option<Arc<StoredResponse>> {
        self.entries.get(fingerprint).cloned()
    }

    pub fn store(
        &mut self,
        fingerprint: RequestFingerprint,
        outcome: crate::replay::TransportOutcome,
        source: ExecutionSource,
    ) {
        self.entries
            .insert(fingerprint, Arc::new(StoredResponse { outcome, source }));
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
                Some(base_arc) if arc.outcome != base_arc.outcome => Some(fp.clone()),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn detect_write_conflicts(
        session: &Self,
        branch: &Self,
        base: &std::collections::HashMap<RequestFingerprint, Arc<StoredResponse>>,
        write_set: &[RequestFingerprint],
    ) -> Vec<RequestFingerprint> {
        write_set
            .iter()
            .filter(|fp| match base.get(*fp) {
                None => match (
                    session.entries.get(*fp).map(|a| &a.outcome),
                    branch.entries.get(*fp).map(|a| &a.outcome),
                ) {
                    (None, _) => false,
                    (Some(session_val), Some(branch_val)) => session_val != branch_val,
                    (Some(_), None) => false,
                },
                Some(base_arc) => {
                    let base_val = Some(&base_arc.outcome);
                    let branch_val = branch.entries.get(*fp).map(|a| &a.outcome);
                    let session_val = session.entries.get(*fp).map(|a| &a.outcome);
                    content_diverged(base_val, branch_val, session_val)
                }
            })
            .cloned()
            .collect()
    }
}

use crate::materialization_conflict::content_diverged;

/// Whether Complete-graph / query-index / response-store consults are lawful (RA-11).
///
/// Completeness is field coverage. This flag is freshness: after an in-session write,
/// a Complete row must not satisfy recorded-read reuse. Copied on fork; not a write-set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum RecordedReadReuse {
    #[default]
    Allowed,
    Invalidated,
}

/// Mutations *performed by this materialization*, distinct from inherited RA-11 reuse.
///
/// Absorb wholesale-replaces graph + auxiliary stores only when this branch itself
/// recorded a write. Inherited [`RecordedReadReuse::Invalidated`] still union-merges
/// so sibling read observations accumulate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BranchLocalWrite {
    #[default]
    None,
    Recorded,
}

/// A Get's resolved input snapshot and row-version witness for scoped reuse.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(crate) struct GetObservation {
    pub capability: plasm_core::CapabilityName,
    pub catalog: String,
    pub environment: IndexMap<String, Value>,
    pub parameters: IndexMap<String, Value>,
    pub authoritative_parameters: Option<Vec<String>>,
    pub row_version: u64,
}

/// Unified per-session materialization state.
#[derive(Debug, Clone, Default)]
pub struct SessionMaterialization {
    pub graph: GraphCache,
    pub responses: SessionResponseStore,
    pub query_index: QueryIndex,
    /// RA-11 freshness of recorded reads. Independent of [`Self::branch_local_write`].
    pub(crate) recorded_read_reuse: RecordedReadReuse,
    /// Whether *this* store ran a mutator. Independent of inherited [`Self::recorded_read_reuse`].
    pub(crate) branch_local_write: BranchLocalWrite,
    /// View and post-write row observations made by this branch, replacing prior membership.
    /// Forks do not inherit this set: inherited rows are not new observations.
    pub(crate) observed_snapshot_rows: std::collections::HashSet<Ref>,
    /// Snapshot identities in the inherited graph; inherited rows are not fresh observations.
    pub(crate) snapshot_row_refs: std::collections::HashSet<Ref>,
    /// Capability params from the fetch that produced each row. Inherited by synthesized GETs.
    pub(crate) inherited_capability_params: std::collections::HashMap<Ref, IndexMap<String, Value>>,
    /// A detail cache hit for an authored read needs the same selected capability and inputs.
    pub(crate) get_observations: std::collections::HashMap<Ref, GetObservation>,
    /// Catalog-keyed fields from action-with-`provides` (e.g. AuthSession login `access_token`).
    /// Overlay onto unary Gets the same way a per-ref stamp / `catalog_bind` already does.
    /// Search selection holes do not read this map.
    pub(crate) provided_session_params: std::collections::HashMap<String, IndexMap<String, Value>>,
    /// RA-17 deployments for RA-6 inherit: omit a parent token onto a foreign seat.
    pub(crate) prerequisite_deployments: DeploymentBindings,
}

impl SessionMaterialization {
    /// Conservative capacity charge for branch-owned container allocations.
    /// Payload strings and nested values are measured separately.
    pub(crate) fn retention_capacity_bytes(&self) -> usize {
        let hash = retained_hash_capacity::<Ref, ()>;
        let mut bytes = self
            .graph
            .retention_capacity_bytes()
            .saturating_add(self.responses.retention_capacity_bytes())
            .saturating_add(self.query_index.retention_capacity_bytes())
            .saturating_add(hash(self.observed_snapshot_rows.capacity()))
            .saturating_add(hash(self.snapshot_row_refs.capacity()))
            .saturating_add(retained_hash_capacity::<Ref, IndexMap<String, Value>>(
                self.inherited_capability_params.capacity(),
            ))
            .saturating_add(retained_hash_capacity::<Ref, GetObservation>(
                self.get_observations.capacity(),
            ))
            .saturating_add(retained_hash_capacity::<String, IndexMap<String, Value>>(
                self.provided_session_params.capacity(),
            ));
        for values in self
            .inherited_capability_params
            .values()
            .chain(self.provided_session_params.values())
        {
            bytes = bytes.saturating_add(value_map_capacity(values, 0));
        }
        for key in self.provided_session_params.keys() {
            bytes = bytes.saturating_add(key.capacity());
        }
        for observation in self.get_observations.values() {
            bytes = bytes
                .saturating_add(observation.catalog.capacity())
                .saturating_add(value_map_capacity(&observation.environment, 0))
                .saturating_add(value_map_capacity(&observation.parameters, 0))
                .saturating_add(
                    observation
                        .authoritative_parameters
                        .as_ref()
                        .map_or(0, |v| {
                            v.capacity().saturating_mul(std::mem::size_of::<String>())
                        }),
                );
            if let Some(parameters) = &observation.authoritative_parameters {
                for parameter in parameters {
                    bytes = bytes.saturating_add(parameter.capacity());
                }
            }
        }
        bytes.saturating_add(
            self.prerequisite_deployments
                .bindings
                .capacity()
                .saturating_mul(std::mem::size_of::<
                    plasm_core::prerequisites::DeploymentBinding,
                >()),
        )
    }
    /// Internal discard-only accounting view, including private cache and
    /// binding values. This is not a public serialization format.
    pub(crate) fn retention_view(&self) -> impl serde::Serialize + '_ {
        (
            self.graph.retention_view(),
            RetainedResponses(&self.responses.entries),
            self.query_index.retention_view(),
            &self.observed_snapshot_rows,
            &self.snapshot_row_refs,
            RetainedPairs(&self.inherited_capability_params),
            RetainedPairs(&self.get_observations),
            &self.provided_session_params,
            &self.prerequisite_deployments,
        )
    }
    pub fn new() -> Self {
        Self::default()
    }

    /// Read-side fan-out / fork seed: same stores and inherited RA-11 reuse; no local write.
    pub(crate) fn seed_read_branch(hot: &Self, graph: GraphCache) -> Self {
        Self {
            graph,
            responses: hot.responses.clone(),
            query_index: hot.query_index.clone(),
            recorded_read_reuse: hot.recorded_read_reuse,
            branch_local_write: BranchLocalWrite::None,
            observed_snapshot_rows: Default::default(),
            snapshot_row_refs: hot.snapshot_row_refs.clone(),
            inherited_capability_params: hot.inherited_capability_params.clone(),
            get_observations: hot.get_observations.clone(),
            provided_session_params: hot.provided_session_params.clone(),
            prerequisite_deployments: hot.prerequisite_deployments.clone(),
        }
    }

    /// Pin RA-17 deployments so relation inherit can prove catalog identity.
    pub fn set_prerequisite_deployments(&mut self, bindings: DeploymentBindings) {
        self.prerequisite_deployments = bindings;
    }

    /// Stamp non-identity capability params (CLI flags, session inherit) onto a row ref.
    /// Path env is projected from [`Ref`] identity; these bindings overlay at CML populate.
    pub fn stamp_capability_params(&mut self, reference: &Ref, params: IndexMap<String, Value>) {
        if params.is_empty() {
            return;
        }
        self.inherited_capability_params
            .entry(reference.clone())
            .or_default()
            .extend(params);
    }

    pub(crate) fn capability_params_for(&self, reference: &Ref) -> IndexMap<String, Value> {
        self.inherited_capability_params
            .get(reference)
            .cloned()
            .unwrap_or_default()
    }

    /// Catalog key for provided-session overlay: explicit stamp, else CGS `entry_id`, else `""`.
    pub(crate) fn provide_catalog_key(cgs: &plasm_core::CGS, stamp: Option<&str>) -> String {
        stamp
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| cgs.entry_id.clone())
            .unwrap_or_default()
    }

    /// Stamp action-`provides` fields for later unary Gets on this catalog.
    /// Survives [`Self::poison_read_caches_after_mutation`] — login is not a cached read.
    pub fn stamp_provided_session_params(
        &mut self,
        catalog_key: impl Into<String>,
        params: IndexMap<String, Value>,
    ) {
        if params.is_empty() {
            return;
        }
        self.provided_session_params
            .entry(catalog_key.into())
            .or_default()
            .extend(params);
    }

    pub(crate) fn provided_session_params_for(&self, catalog_key: &str) -> IndexMap<String, Value> {
        self.provided_session_params
            .get(catalog_key)
            .cloned()
            .unwrap_or_default()
    }

    /// Get CML overlay: catalog-provided session fields, then per-ref stamps (per-ref wins).
    pub(crate) fn capability_params_for_get(
        &self,
        reference: &Ref,
        catalog_key: &str,
    ) -> IndexMap<String, Value> {
        let mut overlay = self.provided_session_params_for(catalog_key);
        overlay.extend(self.capability_params_for(reference));
        if let Some(observation) = self.get_observations.get(reference) {
            if observation.catalog == catalog_key {
                if let Some(names) = &observation.authoritative_parameters {
                    overlay.retain(|name, _| !names.contains(name));
                    overlay.extend(observation.parameters.clone());
                }
            }
        }
        overlay
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

    /// Preserve an authoritative observation through branch absorption. Inherited
    /// copies must not union removed members back into the refreshed relation.
    pub(crate) fn publish_fresh_row(&mut self, row: CachedEntity) -> Result<(), RuntimeError> {
        let reference = row.reference.clone();
        self.graph.overwrite(row)?;
        self.get_observations.remove(&reference);
        self.snapshot_row_refs.insert(reference.clone());
        self.observed_snapshot_rows.insert(reference);
        Ok(())
    }

    pub fn invalidate_after_mutation(&mut self, entity_type: &str) {
        self.query_index.invalidate_entity_type(entity_type);
        self.responses.invalidate_entity_type(entity_type);
    }

    /// After any mutation, composed views and unary Gets must not consult pre-write
    /// scoped caches, response fingerprints, or replay cassettes (RA-11).
    pub fn poison_read_caches_after_mutation(&mut self) {
        self.recorded_read_reuse = RecordedReadReuse::Invalidated;
        self.branch_local_write = BranchLocalWrite::Recorded;
        self.query_index = QueryIndex::default();
        self.responses = SessionResponseStore::default();
        self.inherited_capability_params
            .retain(|reference, _| self.graph.get(reference).is_some());
    }

    /// RA-11: recorded Get / HTTP reuse is lawful only before the first in-session write.
    #[must_use]
    pub fn allows_recorded_read_reuse(&self) -> bool {
        matches!(self.recorded_read_reuse, RecordedReadReuse::Allowed)
    }

    /// Unary Get graph consult. After poison, returns `None` so Get re-observes live.
    #[must_use]
    pub fn consult_complete_get(&self, reference: &Ref) -> Option<&CachedEntity> {
        if !self.allows_recorded_read_reuse() {
            return None;
        }
        self.get(reference)
            .filter(|entity| entity.completeness == EntityCompleteness::Complete)
    }

    /// Evict every cached row of each `invalidates_entities` type and drop scoped query /
    /// response entries so composed primary_read re-fetches live.
    ///
    /// Type-wide eviction is mandatory: mutator echoes may decode under an empty or
    /// wrong-keyed [`Ref`] (e.g. method params carry `access_token` while
    /// `implicit_request_identity` identity never lands on the echo Ref). Surgical
    /// remove-by-decoded-id would miss the seed Get row and leave iterate…until
    /// re-observe satisfied from a stale Completeness::Complete graph hit.
    pub fn apply_post_mutation_cache_effects(
        &mut self,
        capability: &plasm_core::schema::CapabilitySchema,
        cgs: &plasm_core::CGS,
    ) -> Result<(), crate::RuntimeError> {
        if capability.invalidates_entities.is_empty() {
            return Ok(());
        }

        for target_type in &capability.invalidates_entities {
            if cgs.get_entity(target_type.as_str()).is_none() {
                continue;
            }
            self.graph
                .invalidate_matching(|e| e.reference.entity_type.as_str() == target_type.as_str());
            self.invalidate_after_mutation(target_type.as_str());
        }
        Ok(())
    }

    /// Merge fanout branch materialization back into the session (graph + response + query index).
    ///
    /// Wholesale replace is keyed on [`BranchLocalWrite::Recorded`] (this branch mutated),
    /// not inherited [`RecordedReadReuse::Invalidated`]. A poisoned parent with two
    /// read-only sibling forks must union-merge so both observations survive absorb.
    pub fn absorb_branch(&mut self, branch: SessionMaterialization) -> Result<usize, RuntimeError> {
        // Retain scope evidence: changed row versions invalidate reuse, not captured inputs.
        if matches!(branch.branch_local_write, BranchLocalWrite::Recorded) {
            let merged = branch.graph.stats().total_entities;
            self.graph = branch.graph;
            self.get_observations = branch.get_observations;
            self.query_index = branch.query_index;
            self.responses = branch.responses;
            self.inherited_capability_params = branch.inherited_capability_params;
            self.provided_session_params = branch.provided_session_params;
            self.recorded_read_reuse = RecordedReadReuse::Invalidated;
            self.branch_local_write = BranchLocalWrite::Recorded;
            self.observed_snapshot_rows
                .extend(branch.observed_snapshot_rows);
            self.snapshot_row_refs = branch.snapshot_row_refs;
            return Ok(merged);
        }
        let mut ordinary_refs: Vec<_> = branch
            .graph
            .all_references()
            .into_iter()
            .filter(|reference| {
                !branch.snapshot_row_refs.contains(*reference)
                    && !self.snapshot_row_refs.contains(*reference)
            })
            .cloned()
            .collect();
        ordinary_refs.sort_by_key(|reference| reference.to_string());
        let ordinary_rows = ordinary_refs
            .iter()
            .filter_map(|reference| branch.graph.get(reference).cloned())
            .collect();
        let mut merged = self.graph.merge(ordinary_rows)?;
        // A view or post-write read is one observed snapshot, not additive pages of an edge.
        // Preserve that replacement through optimistic branch commit.
        let snapshot_parameters = branch.observed_snapshot_rows.clone();
        for reference in branch.observed_snapshot_rows {
            if let Some(row) = branch.graph.get(&reference) {
                self.publish_fresh_row(row.clone())?;
                if let (Some(mut observation), Some(stored)) = (
                    branch.get_observations.get(&reference).cloned(),
                    self.graph.get(&reference),
                ) {
                    if observation.row_version == row.version {
                        observation.row_version = stored.version;
                        self.get_observations.insert(reference.clone(), observation);
                    }
                }
                merged += 1;
            }
        }
        self.responses.merge_from(branch.responses);
        self.query_index.merge_from(branch.query_index);
        if matches!(branch.recorded_read_reuse, RecordedReadReuse::Invalidated) {
            self.recorded_read_reuse = RecordedReadReuse::Invalidated;
        }
        for (reference, params) in branch.inherited_capability_params {
            if snapshot_parameters.contains(&reference) {
                self.inherited_capability_params.insert(reference, params);
            } else {
                self.stamp_capability_params(&reference, params);
            }
        }
        for (catalog_key, params) in branch.provided_session_params {
            self.stamp_provided_session_params(catalog_key, params);
        }
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
    Inline(Vec<plasm_core::ValueRow>),
    /// Hot graph + spilled pages, resolved in the recorded membership order.
    ///
    /// `hot_snapshot` is captured at materialize time so compute can rehydrate without
    /// re-acquiring the session graph mutex or re-copying hot entities.
    GraphBacked {
        entity_type: String,
        membership: plasm_core::collection_codec::RecordedCollection<plasm_core::Ref>,
        hot_snapshot: std::sync::Arc<[CachedEntity]>,
    },
}

impl MaterializedRowSource {
    pub fn inline_rows(&self) -> Option<&[plasm_core::ValueRow]> {
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
        environment: &plasm_compile::CmlEnv,
        telemetry: &mut CacheTelemetry,
    ) -> Option<(
        plasm_core::collection_codec::RecordedCollection<Ref>,
        Vec<CachedEntity>,
    )> {
        let key = QueryCacheKey::from_query(
            query,
            capability_name,
            cgs,
            environment,
            snapshot.inner.stats().version,
        )
        .ok()?;
        let record = query_index.get(&key)?;
        let refs = record.observed();
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
        Some((record.clone(), entities))
    }

    pub fn record_query_network(telemetry: &mut CacheTelemetry) {
        telemetry.query_required_network += 1;
    }

    pub fn index_query_result(
        mat: &mut SessionMaterialization,
        query: &QueryExpr,
        capability_name: &str,
        entities: &[CachedEntity],
        cgs: &CGS,
        environment: &plasm_compile::CmlEnv,
    ) -> Result<plasm_core::collection_codec::RecordedCollection<Ref>, RuntimeError> {
        use plasm_core::collection_codec::{CollectionCodec, PageTermination, RecordingCodec};
        let key = QueryCacheKey::from_query(
            query,
            capability_name,
            cgs,
            environment,
            mat.graph.stats().version,
        )?;
        let refs: Vec<Ref> = entities.iter().map(|e| e.reference.clone()).collect();
        let codec = RecordingCodec::new();
        let mut acquisition = codec.acquire(key.identity().clone());
        acquisition.push(
            key.identity(),
            0,
            refs.into(),
            entities.len(),
            PageTermination::Exhausted,
        )?;
        let record = acquisition.finish();
        mat.query_index.insert(key, record.clone())?;
        Ok(record)
    }
}

fn client_side_predicate_matches_entity(
    entity: &CachedEntity,
    pred: &plasm_core::Predicate,
    _entity_def: &plasm_core::EntityDef,
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
            .all(|c| client_side_predicate_matches_entity(entity, c, _entity_def)),
        _ => false,
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

    #[tokio::test]
    async fn graph_retention_counts_hidden_relation_backing_rows_and_fork_base() {
        use crate::execution::effect_continuation::{
            EffectContinuation, EffectContinuationError, MAX_EFFECT_SELECTION_BYTES,
        };
        use plasm_core::collection_codec::{
            CollectionCodec, CollectionIdentity, Observation, RecordingCodec, Transform,
        };
        use plasm_core::row_contract::RelationMembership;

        let codec = RecordingCodec::new();
        let identity = CollectionIdentity::for_untyped_observation(&"relation-retention").unwrap();
        let original = codec
            .record(
                identity.clone(),
                vec![
                    Ref::new("Child", "small"),
                    Ref::new("Child", "x".repeat(MAX_EFFECT_SELECTION_BYTES)),
                ],
                Observation::Literal,
            )
            .unwrap();
        let visible = codec
            .derive(
                identity.derived(&"take-one").unwrap(),
                &[&original],
                Transform::Take(1),
            )
            .unwrap();
        let mut entity = CachedEntity::new(Ref::new("Parent", "one"), 0);
        entity
            .relations
            .insert("children".into(), RelationMembership::from_record(visible));
        let mut graph = GraphCache::new();
        graph.overwrite(entity).unwrap();
        let mut branch = graph.fork_for_branch();
        branch
            .overwrite(CachedEntity::new(Ref::new("Parent", "one"), 0))
            .unwrap();
        let (control, _pauses) =
            EffectContinuation::new_native(std::time::Duration::from_secs(2)).unwrap();
        control
            .scope(async {
                for state in [&graph, &branch] {
                    assert!(matches!(
                        EffectContinuation::reserve_retained(&state.retention_view()),
                        Err(EffectContinuationError::SelectionBudget(
                            plasm_core::ValueBudgetError::BytesExceeded
                        ))
                    ));
                }
            })
            .await;
    }

    #[tokio::test]
    async fn graph_and_binding_payload_capacities_are_charged() {
        use crate::execution::effect_continuation::{
            EffectContinuation, EffectContinuationError, MAX_EFFECT_SELECTION_BYTES,
        };
        let mut graph = SessionMaterialization::new();
        let mut entity = CachedEntity::new(Ref::new("Thing", "one"), 0);
        let mut text = String::with_capacity(MAX_EFFECT_SELECTION_BYTES + 1);
        text.push('x');
        entity
            .fields
            .insert("field".into(), plasm_core::TypedFieldValue::String(text));
        graph.graph.overwrite(entity).unwrap();
        let mut binding = SessionMaterialization::new();
        let values = vec![(
            "field".into(),
            Value::Array(Vec::with_capacity(
                MAX_EFFECT_SELECTION_BYTES / std::mem::size_of::<Value>() + 1,
            )),
        )]
        .into_iter()
        .collect();
        binding
            .provided_session_params
            .insert("catalog".into(), values);
        let (control, _pauses) =
            EffectContinuation::new_native(std::time::Duration::from_secs(2)).unwrap();
        control
            .scope(async {
                for state in [&graph, &binding] {
                    let mut reservation =
                        EffectContinuation::reserve_retained(&state.retention_view()).unwrap();
                    assert!(matches!(
                        reservation.add_capacity_bytes(state.retention_capacity_bytes()),
                        Err(EffectContinuationError::SelectionBudget(
                            plasm_core::ValueBudgetError::BytesExceeded
                        ))
                    ));
                }
            })
            .await;
    }

    #[test]
    fn response_capacity_walk_counts_arrays_and_limits_depth() {
        let array = Vec::<serde_json::Value>::with_capacity(100);
        assert!(
            json_retention_capacity(&serde_json::Value::Array(array), 0)
                >= 100 * std::mem::size_of::<serde_json::Value>()
        );
        let mut nested = serde_json::Value::Null;
        for _ in 0..128 {
            nested = serde_json::Value::Array(vec![nested]);
        }
        assert_eq!(json_retention_capacity(&nested, 0), usize::MAX);
    }

    #[tokio::test]
    async fn retained_response_payload_capacity_is_charged() {
        use crate::execution::effect_continuation::{
            EffectContinuation, EffectContinuationError, MAX_EFFECT_SELECTION_BYTES,
        };
        let mut payload = String::with_capacity(MAX_EFFECT_SELECTION_BYTES + 1);
        payload.push('x');
        let mut materialization = SessionMaterialization::new();
        materialization.responses.store(
            RequestFingerprint::from_hex(&format!("{:064x}", 42u64)).unwrap(),
            crate::replay::TransportOutcome {
                response: serde_json::Value::Array(vec![serde_json::Value::String(payload)]),
                next_link: None,
            },
            ExecutionSource::Cache,
        );
        let (control, _pauses) =
            EffectContinuation::new_native(std::time::Duration::from_secs(2)).unwrap();
        control
            .scope(async {
                let mut reservation =
                    EffectContinuation::reserve_retained(&materialization.retention_view())
                        .unwrap();
                assert!(matches!(
                    reservation.add_capacity_bytes(materialization.retention_capacity_bytes()),
                    Err(EffectContinuationError::SelectionBudget(
                        plasm_core::ValueBudgetError::BytesExceeded
                    ))
                ));
            })
            .await;
    }

    #[tokio::test]
    async fn empty_response_store_capacity_is_not_hidden_from_continuation_budget() {
        use crate::execution::effect_continuation::{
            EffectContinuation, EffectContinuationError, MAX_EFFECT_SELECTION_BYTES,
        };
        let mut materialization = SessionMaterialization::new();
        materialization.responses.entries.reserve(110_000);
        let fingerprint = RequestFingerprint::from_hex(&format!("{:064x}", 91u64)).unwrap();
        materialization.responses.store(
            fingerprint,
            crate::replay::TransportOutcome {
                response: serde_json::json!({"ok": true}),
                next_link: None,
            },
            ExecutionSource::Cache,
        );
        materialization.responses.invalidate_entity_type("Item");
        assert!(materialization.responses.entries.is_empty());
        assert!(materialization.retention_capacity_bytes() > MAX_EFFECT_SELECTION_BYTES);
        let (control, _pauses) =
            EffectContinuation::new_native(std::time::Duration::from_secs(2)).unwrap();
        control
            .scope(async {
                let mut reservation =
                    EffectContinuation::reserve_retained(&materialization.retention_view())
                        .unwrap();
                assert!(matches!(
                    reservation.add_capacity_bytes(materialization.retention_capacity_bytes()),
                    Err(EffectContinuationError::SelectionBudget(
                        plasm_core::ValueBudgetError::BytesExceeded
                    ))
                ));
                drop(reservation);
                assert!(EffectContinuation::reserve_retained(&"small").is_ok());
            })
            .await;
    }
    use crate::execution::ExecutionStats;
    use plasm_core::{EntityName, Predicate, QueryExpr, Value};

    #[test]
    fn post_mutation_evicts_stale_read_model_graph_rows() {
        use crate::cache::EntityCompleteness;
        use plasm_core::Ref;

        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");

        let cap = cgs
            .get_capability("langcursor_tick")
            .expect("langcursor_tick on plasm_language_matrix");
        assert!(
            cap.invalidates_entities.iter().any(|e| e == "LangCursor"),
            "fixture must declare type-wide LangCursor eviction"
        );

        let mut mat = SessionMaterialization::new();
        mat.insert(CachedEntity::from_decoded(
            Ref::new("LangCursor", "c1"),
            [
                ("id".into(), Value::String("c1".into())),
                ("phase".into(), Value::String("open".into())),
            ]
            .into_iter()
            .collect(),
            indexmap::IndexMap::new(),
            1,
            EntityCompleteness::Complete,
        ))
        .unwrap();
        let key = QueryCacheKey::test("LangCursor\0get\0id=c1");
        mat.query_index
            .insert_test_observation(key.clone(), vec![Ref::new("LangCursor", "c1")]);

        // Type-wide eviction ignores mutator echo identity; only invalidates_entities matters.
        mat.apply_post_mutation_cache_effects(cap, &cgs).unwrap();

        assert!(mat.get(&Ref::new("LangCursor", "c1")).is_none());
        assert!(mat.query_index.get(&key).is_none());
    }

    #[test]
    fn post_mutation_type_wide_evicts_even_when_echo_ref_is_empty() {
        use crate::cache::EntityCompleteness;
        use plasm_core::Ref;

        let cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .expect("language matrix CGS");

        let cap = cgs
            .get_capability("langcursor_tick")
            .expect("langcursor_tick on plasm_language_matrix");

        let mut mat = SessionMaterialization::new();
        mat.insert(CachedEntity::from_decoded(
            Ref::new("LangCursor", "seed-cursor"),
            [
                ("id".into(), Value::String("seed-cursor".into())),
                ("phase".into(), Value::String("open".into())),
            ]
            .into_iter()
            .collect(),
            indexmap::IndexMap::new(),
            1,
            EntityCompleteness::Complete,
        ))
        .unwrap();

        // Former surgical-by-echo-id path would miss this seed when the mutator decoded
        // under an empty Ref; type-wide eviction clears the whole entity type.
        mat.apply_post_mutation_cache_effects(cap, &cgs).unwrap();

        assert!(mat.get(&Ref::new("LangCursor", "seed-cursor")).is_none());
    }

    #[test]
    fn ra11_consult_complete_get_skips_after_mutation_poison() {
        let mut mat = SessionMaterialization::new();
        let reference = Ref::new("Widget", "w1");
        mat.insert(CachedEntity::from_decoded(
            reference.clone(),
            [("label".into(), Value::String("seed".into()))]
                .into_iter()
                .collect(),
            indexmap::IndexMap::new(),
            1,
            EntityCompleteness::Complete,
        ))
        .unwrap();
        assert!(mat.allows_recorded_read_reuse());
        assert!(mat.consult_complete_get(&reference).is_some());

        mat.poison_read_caches_after_mutation();
        assert!(!mat.allows_recorded_read_reuse());
        assert!(mat.consult_complete_get(&reference).is_none());
        assert!(
            mat.get(&reference).is_some(),
            "poison skips Get consult; it does not drop the graph row"
        );
    }

    #[test]
    fn seed_read_branch_inherits_reuse_not_local_write() {
        let mut hot = SessionMaterialization::new();
        hot.poison_read_caches_after_mutation();
        assert!(matches!(hot.branch_local_write, BranchLocalWrite::Recorded));
        let branch = SessionMaterialization::seed_read_branch(&hot, hot.graph.fork_for_branch());
        assert!(!branch.allows_recorded_read_reuse());
        assert!(matches!(branch.branch_local_write, BranchLocalWrite::None));
        assert!(matches!(
            hot.recorded_read_reuse,
            RecordedReadReuse::Invalidated
        ));
    }

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
            crate::replay::TransportOutcome {
                response: serde_json::json!({"ok": true}),
                next_link: None,
            },
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
        ExecutionCacheConsult::index_query_result(
            &mut mat,
            &q,
            "issue_label_query",
            &[entity],
            &CGS::new(),
            &Default::default(),
        )
        .unwrap();
        let snapshot = mat.snapshot();
        let mut telemetry = CacheTelemetry::default();
        let served = ExecutionCacheConsult::decide_query(
            &q,
            "issue_label_query",
            &snapshot,
            &mat.query_index,
            &CGS::new(),
            &Default::default(),
            &mut telemetry,
        )
        .expect("indexed query should consult-hit");
        assert_eq!(served.1.len(), 1);
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

    #[test]
    fn response_store_brand_new_idempotent_no_conflict() {
        let mut session = SessionResponseStore::default();
        let fp = RequestFingerprint::from_hex(&format!("{:064x}", 42u64)).expect("fp");
        let base = session.entries_snapshot();
        let mut branch = session.clone();
        branch.store(
            fp.clone(),
            crate::replay::TransportOutcome {
                response: serde_json::json!({"rows": [1, 2]}),
                next_link: None,
            },
            ExecutionSource::Live,
        );
        session.store(
            fp.clone(),
            crate::replay::TransportOutcome {
                response: serde_json::json!({"rows": [1, 2]}),
                next_link: None,
            },
            ExecutionSource::Live,
        );
        let write_set = branch.branch_write_fingerprints(&base);
        let conflicts =
            SessionResponseStore::detect_write_conflicts(&session, &branch, &base, &write_set);
        assert!(
            conflicts.is_empty(),
            "identical brand-new fingerprint is not a conflict"
        );
    }

    #[test]
    fn response_store_continuation_difference_conflicts() {
        let mut session = SessionResponseStore::default();
        let fingerprint = RequestFingerprint::from_hex(&format!("{:064x}", 44u64)).unwrap();
        let base = session.entries_snapshot();
        let mut branch = session.clone();
        let response = serde_json::json!({"rows": [1]});
        branch.store(
            fingerprint.clone(),
            crate::replay::TransportOutcome {
                response: response.clone(),
                next_link: Some("https://example.test/next".into()),
            },
            ExecutionSource::Live,
        );
        session.store(
            fingerprint.clone(),
            crate::replay::TransportOutcome {
                response,
                next_link: None,
            },
            ExecutionSource::Live,
        );
        let writes = branch.branch_write_fingerprints(&base);
        assert_eq!(
            SessionResponseStore::detect_write_conflicts(&session, &branch, &base, &writes),
            vec![fingerprint]
        );
    }

    #[test]
    fn response_store_brand_new_divergent_conflict() {
        let mut session = SessionResponseStore::default();
        let fp = RequestFingerprint::from_hex(&format!("{:064x}", 43u64)).expect("fp");
        let base = session.entries_snapshot();
        let mut branch = session.clone();
        branch.store(
            fp.clone(),
            crate::replay::TransportOutcome {
                response: serde_json::json!({"rows": [1]}),
                next_link: None,
            },
            ExecutionSource::Live,
        );
        session.store(
            fp.clone(),
            crate::replay::TransportOutcome {
                response: serde_json::json!({"rows": [2]}),
                next_link: None,
            },
            ExecutionSource::Live,
        );
        let write_set = branch.branch_write_fingerprints(&base);
        let conflicts =
            SessionResponseStore::detect_write_conflicts(&session, &branch, &base, &write_set);
        assert_eq!(conflicts, vec![fp]);
    }
}
