use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum PathValueContext {
    #[error("path variable {name}")]
    Variable { name: String },
    #[error("path conditional branch")]
    ConditionalBranch,
}

#[derive(Error, Debug, Clone)]
pub enum CmlEvaluationError {
    #[error("trim requires a string or null")]
    TrimInputType,
    #[error("field projection requires an object or null")]
    FieldProjectionInputType,
    #[error("local expression name must not be empty")]
    EmptyLocalName,
    #[error("local expression {name} cannot shadow an input")]
    LocalNameShadowsInput { name: String },
    #[error("assertion code must be a nonempty identifier")]
    InvalidAssertionCode,
    #[error("assertion failed: {code}")]
    AssertionFailed { code: String },
    #[error("expected a boolean condition, got {actual}")]
    ConditionType { actual: &'static str },
    #[error("{context} must evaluate to string or number")]
    PathValueType { context: PathValueContext },
    #[error("bearer input must be a nonempty token string")]
    BearerInputType,
    #[error("bearer input contains invalid token characters")]
    BearerTokenCharacters,
    #[error("request headers must be an object")]
    HeadersType,
    #[error("body_format multipart cannot be combined with body")]
    MultipartBodyConflict,
    #[error("body_format multipart requires a multipart declaration")]
    MultipartDeclarationMissing,
    #[error("multipart declaration must contain at least one part")]
    MultipartDeclarationEmpty,
    #[error("multipart part name must be nonempty")]
    MultipartPartNameEmpty,
    #[error("multipart request has no parts after null-valued parts are omitted")]
    MultipartNoRenderedParts,
    #[error("multipart declaration requires multipart body format")]
    MultipartFormatRequired,
    #[error("authentication and headers both declare Authorization")]
    AuthorizationHeaderConflict,
    #[error("credential authentication cannot be combined with an Authorization header")]
    CredentialAuthorizationHeaderConflict,
    #[error("credential source requires an opaque reference")]
    CredentialReferenceType,
    #[error("unbound program operand reached CML encoding")]
    UnboundProgramOperand,
    #[error(transparent)]
    Money(#[from] plasm_core::MoneyError),
    #[error(transparent)]
    Temporal(#[from] plasm_core::temporal::TemporalPatternError),
}

#[derive(Error, Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    #[error("http")]
    Http,
    #[error("graphql")]
    GraphQl,
    #[error("view")]
    View,
    #[error("credential_bind")]
    CredentialBind,
    #[error("evm_call")]
    EvmCall,
    #[error("evm_logs")]
    EvmLogs,
}

#[derive(Error, Debug, Clone)]
pub enum CmlError {
    #[error(transparent)]
    MissingCapabilityMapping(#[from] plasm_core::schema::MissingCapabilityMapping),
    #[error(transparent)]
    ViewNodeResolution(#[from] plasm_core::schema::ViewNodeResolutionError),
    #[error(transparent)]
    FormatTemplate(#[from] crate::FormatTemplateError),

    #[error(transparent)]
    ExpressionValidation(#[from] crate::ExpressionValidationError),

    #[error(transparent)]
    CredentialBind(#[from] crate::CredentialBindError),

    #[error(transparent)]
    UrlProjection(#[from] crate::UrlProjectionError),

    #[error(transparent)]
    Evaluation(#[from] CmlEvaluationError),

    #[error("unsupported CML transport {transport}")]
    UnsupportedTransport { transport: String },

    #[error("invalid {transport} transport template: {source}")]
    InvalidTransportTemplate {
        transport: TransportKind,
        #[source]
        source: std::sync::Arc<serde_json::Error>,
    },

    #[error(
        "Variable '{name}' not found in environment — bind a prior row field or a quoted teaching literal; a bare name is not in scope"
    )]
    VariableNotFound { name: String },

    #[error(transparent)]
    Mail(#[from] crate::MailError),

    #[cfg(feature = "evm")]
    #[error(transparent)]
    Evm(#[from] crate::evm_transport::EvmCompileError),
}
