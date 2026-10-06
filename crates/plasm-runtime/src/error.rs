pub use crate::chain_error::ChainError;
pub use crate::http_wire_error::HttpWireError;
pub use crate::preflight_error::PreflightError;
pub use crate::request_error::{
    HttpStatusFailure, MockServerOperation, RateLimitCause, RequestFailure,
};
pub use crate::response_narrow_error::{ResponseNarrowError, ResponseNarrowHint};
pub use crate::view_plan_error::ViewPlanError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum HttpLimiterError {
    #[error("backend HTTP limiter lock poisoned")]
    LockPoisoned,
    #[error("conflicting HTTP concurrency limits: existing {existing}, requested {requested}")]
    ConflictingLimits { existing: usize, requested: usize },
    #[error("invalid HTTP backend limiter destination: {0}")]
    Destination(#[from] url::ParseError),
    #[error("HTTP concurrency semaphore closed: {0}")]
    Closed(#[from] tokio::sync::AcquireError),
}

#[derive(Error, Debug)]
pub enum PaginationFault {
    #[error("pagination query key `{key}` is not valid for {transport:?}")]
    QueryParameterUnsupported {
        key: String,
        transport: PaginationTransport,
    },
    #[error("initial-only pagination query must compile to an object")]
    InitialQueryObjectRequired,
    #[error("initial-only pagination query requires HTTP transport")]
    InitialQueryHttpRequired,
    #[error("block-range pagination is not valid for {transport:?}")]
    BlockRangeUnsupported { transport: PaginationTransport },
    #[error("pagination body injection requires a JSON object request body")]
    BodyObjectRequired,
    #[error("pagination body_merge_path: expected object at segment '{segment}'")]
    BodyPathObjectRequired { segment: String },
    #[error("expected JSON object in paginated API response")]
    ResponseObjectRequired,
    #[error("pagination response_prefix: missing segment '{segment}'")]
    ResponseSegmentMissing { segment: String },
    #[error("block_range pagination requires a starting block")]
    FromBlockMissing,
    #[error("block_range pagination with --all requires a final block")]
    ToBlockMissing,
    #[error("pagination with location body is not supported for multipart HTTP requests")]
    MultipartUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaginationTransport {
    EvmCall,
    EvmLogs,
    ComposedView,
}

#[derive(Error, Debug)]
pub enum AuthenticationError {
    #[error("required secret '{key}' is not set")]
    SecretMissing { key: String },
    #[error("environment variable '{key}' is empty or whitespace-only")]
    SecretEmpty { key: String },
    #[error("hosted credential '{key}' is not available ({context})")]
    HostedCredentialMissing { key: String, context: &'static str },
    #[error("hosted credential '{key}' is empty or whitespace-only ({context})")]
    HostedCredentialEmpty { key: String, context: &'static str },
    #[error("missing credential for {context} (expected env or hosted_kv)")]
    CredentialSlotMissing { context: &'static str },
    #[error("hosted bearer credential is empty or whitespace-only")]
    HostedBearerEmpty,
    #[error("OAuth access token expired and no refresh_token is stored; re-link the account")]
    RefreshTokenMissing,
    #[error("OAuth access token expired; hosted refresh requires plasm")]
    HostedRefreshUnavailable,
    #[error("OAuth2 response missing 'access_token' field")]
    AccessTokenMissing,
    #[error("OAuth HTTP {operation:?} failed: {source}")]
    Http {
        operation: OAuthHttpOperation,
        #[source]
        source: reqwest::Error,
    },
    #[error("OAuth token endpoint returned HTTP {status}: {source}")]
    TokenEndpoint {
        status: u16,
        #[source]
        source: crate::hosted_oauth_kv::OAuthTokenEndpointError,
    },
    #[error("invalid hosted OAuth credential: {0}")]
    HostedCredential(#[from] crate::hosted_oauth_kv::OutboundOAuthKvParseError),
    #[error("invalid OAuth token response: {0}")]
    TokenResponse(#[from] crate::hosted_oauth_kv::ApplyTokenError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthHttpOperation {
    BuildClient,
    SendTokenRequest,
    DecodeTokenResponse,
}

#[derive(Error, Debug)]
pub enum CacheError {
    #[error("entity not found: {reference}")]
    EntityMissing { reference: plasm_core::Ref },
    #[error("cannot merge different references: {expected} vs {actual}")]
    MergeReferenceMismatch {
        expected: plasm_core::Ref,
        actual: plasm_core::Ref,
    },
    #[error("unobserved relation {reference}.{relation}")]
    RelationUnobserved {
        reference: plasm_core::Ref,
        relation: String,
    },
    #[error("composed view `{view}` returned no entity row")]
    ViewRowMissing { view: String },
    #[error("unexpected transport request: {method:?} {path}")]
    UnexpectedRequest {
        method: plasm_compile::HttpMethod,
        path: String,
    },
    #[error("transport detail unavailable")]
    DetailUnavailable,
    #[error("harness note not found: {id}")]
    HarnessNoteMissing { id: String },
}

#[derive(Error, Debug)]
pub enum SerializationError {
    #[error("JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("multipart JSON encode for `{part}` failed: {source}")]
    MultipartJson {
        part: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("form-urlencoded encode failed: {0}")]
    Form(#[from] serde_urlencoded::ser::Error),
    #[error("money encoding failed: {0}")]
    Money(#[from] plasm_core::MoneyError),
}

#[derive(Error, Debug)]
pub enum ReplayStoreError {
    #[error("invalid hex fingerprint: {0}")]
    FingerprintHex(#[from] hex::FromHexError),
    #[error("fingerprint must be 32 bytes (received {actual})")]
    FingerprintLength { actual: usize },
    #[error("replay storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("replay storage {operation:?} failed: {source}")]
    Operation {
        operation: ReplayStoreOperation,
        #[source]
        source: std::io::Error,
    },
    #[error("replay entry JSON failed: {0}")]
    EntryJson(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayStoreOperation {
    CreateDirectory,
    WriteEntry,
    ReadEntry,
    ListEntries,
    ReadDirectoryEntry,
}

#[derive(Error, Debug)]
pub enum RuntimeError {
    #[error("response narrowing failed: {0}")]
    ResponseNarrowing(#[from] ResponseNarrowError),
    #[error("host transport failed: {source}")]
    HostTransport {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("HTTP wire encoding failed: {0}")]
    HttpWire(#[from] HttpWireError),
    #[error("chain traversal failed: {0}")]
    Chain(#[from] ChainError),
    #[error("view planning failed: {0}")]
    ViewPlan(#[from] ViewPlanError),
    #[error("preflight failed: {0}")]
    Preflight(#[from] PreflightError),
    #[error("EVM execution failed: {0}")]
    Evm(#[from] crate::evm::EvmError),
    #[error("HTTP limiter failed: {0}")]
    HttpLimiter(#[from] HttpLimiterError),
    #[error("pagination configuration failed: {0}")]
    PaginationFault(#[from] PaginationFault),
    #[error("relation hop missing required parameters: {params:?}; inherit required bindings from the parent Get before traversing the relation")]
    RelationParametersMissing { params: Vec<String> },
    #[error("get_scoped_bindings missing bound value for `{field}` on entity `{entity}`")]
    MaterializeBindingMissing { entity: String, field: String },
    #[error("hydrate_from_embed_path requires plan materialization")]
    EmbeddedHydrationPlanRequired,
    #[error("read-backed identity must be a string or integer")]
    ReadIdentityType,
    #[error("execution requires an explicitly compiled catalog")]
    CompiledCatalogMissing,
    #[error("execution requires a pinned compiled catalog scope")]
    CompiledCatalogScopeMissing,
    #[error("composed views execute via Query, not HTTP invoke")]
    ViewQueryDispatchRequired,
    #[error("EVM transport requires an RPC base URL")]
    EvmRpcUrlMissing,
    #[error("continuation {kind:?} must execute through its host dispatcher")]
    ContinuationDispatchRequired { kind: ContinuationKind },
    #[error("teaching values cannot be executed")]
    TeachingValueNotExecutable,
    #[error("capability `{capability}` not found")]
    CapabilityUnknown { capability: String },
    #[error("capability `{capability}` must support reads (got {actual:?})")]
    ReadCapabilityRequired {
        capability: String,
        actual: plasm_core::CapabilityKind,
    },
    #[error("capability `{capability}` must be kind get on `{entity}`")]
    GetCapabilityRequired { capability: String, entity: String },
    #[error("derived Get `{capability}` cannot nest inside a view DAG")]
    DerivedGetNestingForbidden { capability: String },
    #[error("composed-view GET cannot nest inside another view DAG")]
    ViewGetNestingForbidden,
    #[error("Get identity mismatch: requested {expected}, returned {actual}")]
    GetIdentityMismatch {
        expected: plasm_core::Ref,
        actual: plasm_core::Ref,
    },
    #[error("query/search capabilities must use HTTP CML templates")]
    HttpQueryTemplateRequired,
    #[error("composed views do not support CML pagination")]
    ViewPaginationUnsupported,
    #[error("pagination stopped after {max_pages} pages (safety cap)")]
    PaginationPageLimit { max_pages: usize },
    #[error("absolute-URL pagination requires live execution")]
    LiveAbsolutePaginationRequired,
    #[error("credential provider failed: {source}")]
    CredentialProvider {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("ordering contract failed: {0}")]
    Ordering(#[from] plasm_core::value_order::OrderingError),
    #[error("top-k sort field is unobserved (not null)")]
    TopKFieldUnobserved,
    #[error("top-k sequence overflow")]
    TopKSequenceOverflow,
    #[error("empty top-k field")]
    TopKFieldEmpty,
    #[error("unknown field `{field}` on entity `{entity}`")]
    FieldUnknown { entity: String, field: String },
    #[error("unknown entity `{entity}`")]
    EntityUnknown { entity: String },
    #[error("view references missing node `{node}`")]
    ViewNodeMissing { node: String },
    #[error("computed output bindings are resolved in a separate phase")]
    ComputedOutputPhaseRequired,
    #[error("traversal parent returned entity `{actual}` instead of `{expected}`")]
    TraversalParentTypeMismatch { expected: String, actual: String },
    #[error("credential contract failed: {0}")]
    Credential(#[from] crate::credentials::CredentialError),
    #[error("pagination contract failed: {0}")]
    PaginationContract(#[from] plasm_compile::PaginationContractError),
    #[error(transparent)]
    CatalogTemplate(#[from] plasm_compile::CatalogTemplateError),
    #[error("HTTP {phase:?} failed: {source}")]
    HttpTransport {
        phase: HttpTransportPhase,
        #[source]
        source: reqwest::Error,
        attempts: u32,
    },
    #[error("identity projection failed: {0}")]
    IdentityProjection(#[from] plasm_core::PathEnvProjectionError),
    #[error("entity reference scope normalization failed: {0}")]
    EntityRefScope(#[from] plasm_core::ScopeEntityRefNormalizeError),
    #[error("query resolution failed: {0}")]
    QueryResolution(#[from] plasm_core::QueryCapabilityResolveError),
    #[error("schema contract failed: {0}")]
    SchemaContract(#[from] plasm_core::SchemaError),
    #[error("operand resolution failed: {0}")]
    OperandResolution(#[from] plasm_core::operand_binding::ResolvedValueError),
    #[error("computed view template {phase:?} failed: {source}")]
    ViewTemplate {
        phase: ViewTemplatePhase,
        #[source]
        source: minijinja::Error,
    },
    #[error("computed view template must be non-empty")]
    ViewTemplateEmpty,
    #[error("computed view template exceeds {max_chars} characters")]
    ViewTemplateTooLong { max_chars: usize },
    #[error("view `{view}` node `{node}` (capability `{capability}`): {source}")]
    ViewNode {
        view: String,
        node: String,
        capability: String,
        #[source]
        source: Box<RuntimeError>,
    },
    #[error(transparent)]
    ViewNodeResolution(#[from] plasm_core::schema::ViewNodeResolutionError),
    #[error("field `{field}` is unavailable on {reference}")]
    FieldUnavailable {
        reference: plasm_core::Ref,
        field: String,
    },
    #[error(transparent)]
    Collection(#[from] plasm_core::collection_codec::CollectionFault),
    #[error(transparent)]
    ValueContract(#[from] plasm_core::value_contract::ValueContractError),
    #[error(transparent)]
    TemporalInput(#[from] plasm_core::temporal_input::TemporalInputError),
    #[error(transparent)]
    ValueCoercion(#[from] plasm_core::CoercionError),
    #[error(transparent)]
    RowPredicate(#[from] crate::row_predicate::RowPredicateError),
    #[error("Compilation error: {source}")]
    CompilationError {
        #[from]
        source: plasm_compile::CompileError,
    },

    #[error("Type error: {source}")]
    TypeError {
        #[from]
        source: plasm_core::TypeError,
    },

    #[error("Decode error: {source}")]
    DecodeError {
        #[from]
        source: plasm_compile::DecodeError,
    },

    #[error("CML error: {source}")]
    CmlError {
        #[from]
        source: plasm_compile::CmlError,
    },

    #[error("HTTP request failed: {source}")]
    RequestError {
        #[source]
        source: RequestFailure,
        attempts: u32,
        status: Option<u16>,
        body: Option<serde_json::Value>,
    },

    #[error("Workflow conflict: {kind} on `{entity}`", kind = .conflict.kind.as_str(), entity = .conflict.entity)]
    WorkflowConflict {
        conflict: Box<plasm_core::WorkflowConflict>,
        attempts: u32,
    },

    #[error("Rate limited (HTTP {status}): {source}")]
    RateLimited {
        status: u16,
        host: String,
        retry_after: Option<std::time::Duration>,
        attempts: u32,
        #[source]
        source: RateLimitCause,
    },

    #[error("Cache error: {0}")]
    CacheError(#[from] CacheError),
    #[error("Cache error: {0}")]
    CacheSource(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("cached value is not a row")]
    CacheValueRow(#[from] plasm_core::ValueRowError),
    #[error("cached row is invalid")]
    CacheRowDecode(#[from] plasm_core::row_contract::RowDecodeError),

    #[error("Execution mode '{mode}' not supported")]
    UnsupportedExecutionMode { mode: String },

    #[error("Capability '{capability}' not found for entity '{entity}'")]
    CapabilityNotFound { capability: String, entity: String },

    #[error("No fingerprint found for request")]
    FingerprintNotFound,

    #[error("Replay entry not found for fingerprint: {fingerprint}")]
    ReplayEntryNotFound { fingerprint: String },

    #[error("Replay store error: {0}")]
    ReplayStoreError(#[from] ReplayStoreError),

    #[error("zero rows — Derived get `{capability}`: no row where {match_field} == {identity:?}")]
    DerivedGetNotFound {
        capability: String,
        match_field: String,
        identity: String,
    },

    #[error(
        "Derived get `{capability}`: {matches} rows match {match_field} == {identity:?} (ambiguous)"
    )]
    DerivedGetNonUnique {
        capability: String,
        match_field: String,
        identity: String,
        matches: usize,
    },

    #[error("Derived get `{capability}`: source field `{field}` missing on matched row")]
    DerivedGetSourceFieldMissing { capability: String, field: String },

    #[error(
        "Derived get `{capability}`: source query did not fully materialize (has_more=true); cannot claim not-found"
    )]
    DerivedGetIncompleteSource { capability: String },

    #[error("pagination progress guard: {reason:?}")]
    PaginationProgress {
        reason: crate::execution::PaginationTerminalReason,
    },

    #[error("Serialization error: {0}")]
    SerializationError(#[from] SerializationError),

    #[error("Authentication error: {0}")]
    AuthenticationError(#[from] AuthenticationError),

    #[error("Execution cancelled")]
    Cancelled,

    #[error("synthesized GET `{cap_name}` during {entity_type} hydration: {source}")]
    HydrationGet {
        cap_name: String,
        entity_type: String,
        #[source]
        source: Box<RuntimeError>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewTemplatePhase {
    BindData,
    Compile,
    Render,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpTransportPhase {
    BuildClient,
    Send,
    ReadResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinuationKind {
    Page,
    Wait,
    Cancel,
}

impl RuntimeError {
    pub fn set_attempts(&mut self, attempts: u32) {
        match self {
            RuntimeError::HttpTransport { attempts: a, .. }
            | RuntimeError::RequestError { attempts: a, .. }
            | RuntimeError::WorkflowConflict { attempts: a, .. }
            | RuntimeError::RateLimited { attempts: a, .. } => *a = attempts,
            RuntimeError::HydrationGet { source, .. } => source.set_attempts(attempts),
            _ => {}
        }
    }

    pub fn request_failure(source: RequestFailure, attempts: u32) -> Self {
        Self::RequestError {
            source,
            attempts,
            status: None,
            body: None,
        }
    }
}

impl From<reqwest::Error> for RuntimeError {
    fn from(err: reqwest::Error) -> Self {
        RuntimeError::HttpTransport {
            phase: HttpTransportPhase::Send,
            source: err.without_url(),
            attempts: 1,
        }
    }
}

impl From<serde_json::Error> for RuntimeError {
    fn from(err: serde_json::Error) -> Self {
        RuntimeError::SerializationError(SerializationError::Json(err))
    }
}

impl From<std::io::Error> for RuntimeError {
    fn from(err: std::io::Error) -> Self {
        RuntimeError::ReplayStoreError(ReplayStoreError::Io(err))
    }
}
