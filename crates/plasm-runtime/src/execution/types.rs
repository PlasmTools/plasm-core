//! Core execution result / config / stream types.

use crate::materialization::CacheTelemetry;
use crate::{AuthResolver, CachedEntity, CancelSignal, RuntimeError};
use plasm_core::PromptPipelineConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

/// Execution modes for the runtime
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Execute against live backend
    Live,
    /// Use recorded responses only
    Replay,
    /// Use replay if available, otherwise live + record
    Hybrid,
}

/// Configuration for the execution engine
#[derive(Debug, Clone)]
pub struct ExecutionConfig {
    /// Base URL or RPC endpoint for live backend requests.
    pub base_url: Option<String>,
    /// Default execution mode
    pub default_mode: ExecutionMode,
    /// HTTP client timeout in seconds
    pub timeout_seconds: u64,
    /// Whether to validate responses after decoding
    pub validate_responses: bool,
    /// Process-wide concurrent outbound HTTP permits (transport semaphore).
    pub max_concurrent_requests: usize,
    /// Per-origin concurrent outbound HTTP permits.
    pub per_host_max_inflight: usize,
    /// Path for the replay store directory (if using replay/hybrid)
    pub replay_store_path: Option<std::path::PathBuf>,
    /// After query, fetch each row via GET when the entity has a Get capability whose
    /// required capability params are inherited from the parent query env (unless
    /// `QueryExpr.hydrate == Some(false)`). Identity-only GETs still hydrate; GETs that
    /// require extra params do not run unless those params are present.
    pub hydrate: bool,
    /// Max concurrent GETs during query hydration.
    pub hydrate_concurrency: usize,
    /// Per-registry-entry outbound concurrency caps (catalog `entry_id` → max inflight
    /// for hydrate / projection / relation fan-out). Empty = use [`Self::hydrate_concurrency`].
    /// Preferred over globally lowering hydrate concurrency when one backend is fragile.
    pub backend_max_inflight: HashMap<String, usize>,
    /// Max attempts per logical HTTP request (including first try).
    pub http_max_attempts: u32,
    pub http_retry_initial_backoff_ms: u64,
    pub http_retry_max_backoff_ms: u64,
    /// Wall-clock retry budget per logical HTTP request.
    pub http_retry_total_budget_ms: u64,
    /// teaching prompt rendering + symbol expansion (REPL `:schema`, HTTP execute session prompt, eval).
    pub prompt_pipeline: PromptPipelineConfig,
}

/// Why [`StreamConsumeOpts::max_items`] / row-match budget was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsumeBoundKind {
    /// No item cap from the host or the expression (fetch-all / unbounded).
    #[default]
    None,
    /// Explicit `| take n` / filter+limit / sort+limit on the requested expression.
    ExpressionTake,
    /// Implicit host first-page cap (not part of the expression).
    HostPage,
}

/// Result of a query execution
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionResult {
    /// The entities returned by the query
    pub entities: Vec<CachedEntity>,
    /// Number of entities in the result
    pub count: usize,
    /// For paginated queries: whether more rows may exist after this materialized batch.
    #[serde(default)]
    pub has_more: bool,
    /// Coverage of this result relative to the requested expression.
    ///
    /// Distinct from [`Self::has_more`] (presentation / opaque paging). A Partial
    /// result stays Partial if a continuation expires. Wire name: `coverage`.
    #[serde(default)]
    pub coverage: super::ResultCoverage,
    /// Host-only continuation payload for opaque LLM paging (`page(pg#)`); never serialized on wire.
    #[serde(skip)]
    pub pagination_resume: Option<QueryPaginationResumeData>,
    /// When set, MCP/HTTP layers may surface a one-line `page(handle)` hint after truncated lists.
    #[serde(skip)]
    pub paging_handle: Option<plasm_core::PagingHandle>,
    /// Whether the result came from cache/replay or live execution
    pub source: ExecutionSource,
    /// Execution statistics
    pub stats: ExecutionStats,
    /// Hex-encoded [`crate::RequestFingerprint`] for each successful outbound compiled op (live or replay), in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_fingerprints: Vec<String>,
    /// HTTP-2 operation acknowledgments (not telemetry). Empty on a pure read.
    #[serde(default)]
    pub operations: super::OperationLedger,
}

impl ExecutionResult {
    /// Stamp coverage on an already-built result (prefer over field mutation at call sites).
    #[must_use]
    pub fn with_coverage(mut self, coverage: super::ResultCoverage) -> Self {
        self.coverage = coverage;
        self
    }

    /// No completeness proof — default for unproven / cache-without-proof paths.
    #[must_use]
    pub fn unproven(mut self) -> Self {
        self.coverage = super::ResultCoverage::Unknown;
        self
    }
}

/// Source of execution result
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionSource {
    Live,
    Replay,
    Cache,
}

impl ExecutionSource {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Replay => "replay",
            Self::Cache => "cache",
        }
    }
}

/// Execution statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionStats {
    /// Duration of execution in milliseconds
    pub duration_ms: u64,
    /// Whether any network requests were made
    pub network_requests: usize,
    /// Cache hits during execution (legacy aggregate; see [`Self::cache`])
    pub cache_hits: usize,
    /// Cache misses during execution (legacy aggregate; see [`Self::cache`])
    pub cache_misses: usize,
    /// Honest consult counters and row materialization count.
    #[serde(default)]
    pub cache: CacheTelemetry,
}

impl ExecutionStats {
    pub fn from_telemetry(telemetry: CacheTelemetry, network_requests: usize) -> Self {
        Self {
            duration_ms: 0,
            network_requests,
            cache_hits: telemetry.legacy_cache_hits(),
            cache_misses: telemetry.legacy_cache_misses(),
            cache: telemetry,
        }
    }

    pub fn merge_telemetry(&mut self, other: &CacheTelemetry) {
        self.cache.merge(other);
        self.cache_hits = self.cache.legacy_cache_hits();
        self.cache_misses = self.cache.legacy_cache_misses();
    }

    pub fn record_rows_materialized(&mut self, count: usize) {
        self.cache.rows_materialized = self.cache.rows_materialized.saturating_add(count);
    }
}

/// Stop paginating once this many rows satisfy [`Self::row_match_budget`] predicates.
#[derive(Debug, Clone)]
pub struct RowMatchBudget {
    pub count: usize,
    pub predicates: Vec<crate::row_predicate::JsonRowPredicate>,
}

/// Out-of-band consumption controls: how many pages / entities to pull (not part of the IR).
#[derive(Debug, Clone, Default)]
pub struct StreamConsumeOpts {
    /// Fetch every page until the API reports completion (bounded by a runtime safety cap).
    pub fetch_all: bool,
    /// Item budget across pages. Expression takes are exact; host paging retains
    /// a complete backend page so continuation cannot skip its remaining rows.
    pub max_items: Option<usize>,
    /// When set with [`Self::max_items`], perform at most **one** upstream HTTP page while still
    /// clamping page size to `max_items` (LLM paging batches). When unset, `max_items` alone spans
    /// multiple upstream pages until the budget is satisfied (CLI `--limit`).
    pub one_page: bool,
    /// When true, paginated reads keep rows in session graph (+ optional spill) only — do not
    /// duplicate every page into [`ExecutionResult::entities`].
    pub graph_backed_result: bool,
    /// Row-level filter budget: keep paginating until `count` matching rows are materialized.
    pub row_match_budget: Option<RowMatchBudget>,
    /// Streaming top-k over all pages (sort+limit pushdown); memory O(k).
    pub top_k: Option<crate::top_k::TopKSpec>,
    /// Distinguishes expression `take` from implicit host paging for coverage.
    pub bound_kind: ConsumeBoundKind,
}

pub type RowsProgressFn = std::sync::Arc<dyn Fn(usize) + Send + Sync>;

/// Snapshot of [`PaginationLoopState`] for opaque LLM paging continuations (host-only; not for wire serde).
#[derive(Debug, Clone, PartialEq)]
pub struct QueryPaginationState {
    pub param_values: Vec<(String, Option<serde_json::Value>)>,
    pub next_absolute_url: Option<String>,
    pub last_requested_limit: u32,
    pub from_block: Option<u64>,
    pub final_to_block: Option<u64>,
    pub last_requested_to_block: Option<u64>,
}

/// Everything needed to issue the next paginated HTTP request after a first-page batch.
/// Host-only snapshot: not serialized on HTTP/MCP wires (avoid accidental logging of templates/env).
#[derive(Debug, Clone, PartialEq)]
pub struct QueryPaginationResumeData {
    pub query: plasm_core::QueryExpr,
    pub capability_name: String,
    pub env: plasm_compile::CmlEnv,
    pub template: plasm_compile::CapabilityTemplate,
    pub config: plasm_compile::PaginationConfig,
    pub state: QueryPaginationState,
}

/// One page of decoded, hydrated query results.
#[derive(Debug, Clone, Serialize)]
pub struct PageResult {
    pub entities: Vec<CachedEntity>,
    pub page_index: usize,
    /// Whether another poll may return more rows (same query / stream).
    pub has_more: bool,
    /// Coverage of this page relative to the requested expression.
    #[serde(default)]
    pub coverage: super::ResultCoverage,
    /// When present, host may mint an opaque `page(pg#)` handle for the next batch.
    #[serde(skip)]
    pub pagination_resume: Option<QueryPaginationResumeData>,
    pub stats: ExecutionStats,
    /// HTTP-2 ledger for this page (writes); empty on a pure read page.
    #[serde(default)]
    pub operations: super::OperationLedger,
}

impl PageResult {
    /// Lift a finished [`ExecutionResult`] into a single stream page, preserving coverage.
    #[must_use]
    pub fn from_execution_result(res: ExecutionResult) -> Self {
        Self {
            entities: res.entities,
            page_index: 0,
            has_more: res.has_more,
            coverage: res.coverage,
            pagination_resume: res.pagination_resume,
            stats: res.stats,
            operations: res.operations,
        }
    }
}

pub type QueryStream<'a> =
    Pin<Box<dyn futures_util::Stream<Item = Result<PageResult, RuntimeError>> + Send + 'a>>;

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            base_url: None,
            default_mode: ExecutionMode::Live,
            timeout_seconds: 30,
            validate_responses: true,
            max_concurrent_requests: 64,
            per_host_max_inflight: 24,
            replay_store_path: None,
            hydrate: true,
            hydrate_concurrency: 16,
            backend_max_inflight: HashMap::new(),
            http_max_attempts: 4,
            http_retry_initial_backoff_ms: 500,
            http_retry_max_backoff_ms: 30_000,
            http_retry_total_budget_ms: 120_000,
            prompt_pipeline: PromptPipelineConfig::default(),
        }
    }
}

impl ExecutionConfig {
    /// Effective hydrate / projection fan-out for a catalog `entry_id`.
    ///
    /// When `PLASM_HTTP_BACKEND_MAX_INFLIGHT` sets a per-backend cap, that limit is applied
    /// (capped by the process hydrate concurrency). Empty / unknown entry ids use the global
    /// hydrate concurrency only.
    #[must_use]
    pub fn effective_hydrate_concurrency(&self, entry_id: Option<&str>) -> usize {
        let global = self.hydrate_concurrency.max(1);
        let Some(id) = entry_id.map(str::trim).filter(|s| !s.is_empty()) else {
            return global;
        };
        match self.backend_max_inflight.get(id).copied() {
            Some(n) if n > 0 => n.min(global),
            _ => global,
        }
    }
}
