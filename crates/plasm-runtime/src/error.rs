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
        expected: Box<plasm_core::Ref>,
        actual: Box<plasm_core::Ref>,
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
    #[error("request-owned identity requires observable resolved authentication")]
    RequestIdentityAuthOpaque,
    #[error("request-owned identity for {entity} requires one response row, got {rows}")]
    RequestIdentityCardinality { entity: String, rows: usize },
    #[error(
        "capability {capability} declares one output entity {entity}, got {rows} response rows"
    )]
    DeclaredOutputCardinality {
        capability: String,
        entity: String,
        rows: usize,
    },
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
    Evm(#[from] Box<crate::evm::EvmError>),
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
        expected: Box<plasm_core::Ref>,
        actual: Box<plasm_core::Ref>,
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
    SchemaContract(#[from] Box<plasm_core::SchemaError>),
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
        source: Box<plasm_core::TypeError>,
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
        body: Option<Box<serde_json::Value>>,
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
    CacheRowDecode(#[from] Box<plasm_core::row_contract::RowDecodeError>),

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

impl From<plasm_core::SchemaError> for RuntimeError {
    fn from(error: plasm_core::SchemaError) -> Self {
        Self::SchemaContract(Box::new(error))
    }
}

impl From<plasm_core::TypeError> for RuntimeError {
    fn from(source: plasm_core::TypeError) -> Self {
        Self::TypeError {
            source: Box::new(source),
        }
    }
}

impl From<crate::evm::EvmError> for RuntimeError {
    fn from(source: crate::evm::EvmError) -> Self {
        Self::Evm(Box::new(source))
    }
}

impl From<plasm_core::row_contract::RowDecodeError> for RuntimeError {
    fn from(source: plasm_core::row_contract::RowDecodeError) -> Self {
        Self::CacheRowDecode(Box::new(source))
    }
}

#[cfg(test)]
mod footprint_tests {
    use super::*;

    #[test]
    fn runtime_and_cache_errors_have_bounded_footprints() {
        // Cross-crate Clippy sees this external enum opaquely: keep the complete
        // enum, including its discriminant, below the 128-byte threshold.
        assert!(
            std::mem::size_of::<RuntimeError>() < 128,
            "RuntimeError occupies {} bytes",
            std::mem::size_of::<RuntimeError>()
        );
        assert!(std::mem::size_of::<CacheError>() < 128);
    }

    #[test]
    fn boxed_type_import_preserves_metadata_chain_and_classification() {
        use std::error::Error;
        let source = plasm_core::TypeError::EntityRefCoercionFailure {
            field: "parent".into(),
            target: "Item".into(),
            source: plasm_core::CoercionError::MissingTemporalFormat,
        };
        let expected_display = format!("Type error: {source}");
        let error = RuntimeError::from(source);
        assert_eq!(error.to_string(), expected_display);
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<Box<plasm_core::TypeError>>()
            .unwrap()
            .as_ref();
        assert!(matches!(source,
            plasm_core::TypeError::EntityRefCoercionFailure { field, target, .. }
                if field == "parent" && target == "Item"));
        assert!(matches!(
            source
                .source()
                .unwrap()
                .downcast_ref::<plasm_core::CoercionError>(),
            Some(plasm_core::CoercionError::MissingTemporalFormat)
        ));
        let failure = crate::ExecutionFailure::from(error);
        assert_eq!(failure.cause, crate::FailureCause::Runtime);
        assert_eq!(failure.code, "runtime_type_violation");
    }

    #[test]
    fn boxed_evm_import_preserves_source_chain_display_and_classification() {
        use std::error::Error;
        let source = crate::evm::EvmError::RpcUrl(url::Url::parse(":").unwrap_err());
        let expected_display = format!("EVM execution failed: {source}");
        let error = RuntimeError::from(source);
        assert_eq!(error.to_string(), expected_display);
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<Box<crate::evm::EvmError>>()
            .unwrap()
            .as_ref();
        assert!(matches!(source, crate::evm::EvmError::RpcUrl(_)));
        assert!(matches!(
            source.source().unwrap().downcast_ref::<url::ParseError>(),
            Some(url::ParseError::RelativeUrlWithoutBase)
        ));
        let failure = crate::ExecutionFailure::from(error);
        assert_eq!(failure.cause, crate::FailureCause::Unclassified);
        assert_eq!(failure.code, "unclassified_execution_failure");
    }

    #[test]
    fn boxed_row_decode_import_preserves_metadata_and_classification() {
        use std::error::Error;
        let error = RuntimeError::from(
            plasm_core::row_contract::RowDecodeError::RelationTargetMismatch {
                relation: "items".into(),
                expected: "Item".into(),
                actual: "Other".into(),
            },
        );
        assert_eq!(error.to_string(), "cached row is invalid");
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<Box<plasm_core::row_contract::RowDecodeError>>()
            .unwrap()
            .as_ref();
        assert!(matches!(source,
            plasm_core::row_contract::RowDecodeError::RelationTargetMismatch { relation, expected, actual }
                if relation == "items" && expected == "Item" && actual == "Other"));
        let failure = crate::ExecutionFailure::from(error);
        assert_eq!(failure.cause, crate::FailureCause::Unclassified);
        assert_eq!(failure.code, "unclassified_execution_failure");
    }

    #[test]
    fn schema_contract_preserves_concrete_source_and_metadata() {
        use std::error::Error;

        let source = plasm_core::SchemaError::UnknownValueDomain {
            key: "missing".into(),
            context: "named_value_for_slot".into(),
        };
        let diagnostic = source.to_string();
        let error = RuntimeError::from(source);
        assert_eq!(
            error.to_string(),
            format!("schema contract failed: {diagnostic}")
        );
        assert!(matches!(
            error.source().unwrap().downcast_ref::<Box<plasm_core::SchemaError>>().map(Box::as_ref),
            Some(plasm_core::SchemaError::UnknownValueDomain { key, context })
                if key == "missing" && context == "named_value_for_slot"
        ));
    }

    #[test]
    fn cache_mismatch_preserves_concrete_source_and_references() {
        use std::error::Error;

        let expected = plasm_core::Ref::new("Record", "expected");
        let actual = plasm_core::Ref::new("Record", "actual");
        let error = RuntimeError::from(CacheError::MergeReferenceMismatch {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual.clone()),
        });
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<CacheError>()
            .unwrap();
        assert_eq!(
            source.to_string(),
            format!("cannot merge different references: {expected} vs {actual}")
        );
        assert!(matches!(
            source,
            CacheError::MergeReferenceMismatch {
                expected: boxed_expected,
                actual: boxed_actual,
            } if boxed_expected.as_ref() == &expected && boxed_actual.as_ref() == &actual
        ));
    }

    #[test]
    fn identity_mismatch_boxes_and_preserves_both_references() {
        let expected = plasm_core::Ref::new("Record", "expected");
        let actual = plasm_core::Ref::new("Record", "actual");
        let error = RuntimeError::GetIdentityMismatch {
            expected: Box::new(expected.clone()),
            actual: Box::new(actual.clone()),
        };
        assert_eq!(
            error.to_string(),
            format!("Get identity mismatch: requested {expected}, returned {actual}")
        );
        let RuntimeError::GetIdentityMismatch {
            expected: boxed_expected,
            actual: boxed_actual,
        } = error
        else {
            panic!("expected identity mismatch");
        };
        assert_eq!(*boxed_expected, expected);
        assert_eq!(*boxed_actual, actual);
        assert_eq!(
            std::mem::size_of_val(&boxed_expected) + std::mem::size_of_val(&boxed_actual),
            2 * std::mem::size_of::<usize>()
        );
    }
}
