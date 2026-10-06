use thiserror::Error;

#[derive(Debug, Error)]
pub enum HttpWireError {
    #[error("multipart request missing compiled multipart.parts")]
    MultipartPartsMissing,
    #[error("multipart body_format requires compiled multipart.parts, not `body`")]
    MultipartBodyForbidden,
    #[error("CML header `{header}` conflicts with resolver-owned authentication header `{resolved_header}`")]
    AuthenticationHeaderConflict {
        header: String,
        resolved_header: String,
    },
    #[error("multipart part `{part}` has invalid content_type: {source}")]
    MultipartContentType {
        part: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("multipart part `{part}` file_name requires attachment or JSON object/array content")]
    MultipartFileNameRequiresBinary { part: String },
    #[error("multipart part `{part}` contains an unbound program operand")]
    MultipartUnboundOperand { part: String },
    #[error("multipart part `{part}` has unexpected null content")]
    MultipartNull { part: String },
    #[error("multipart part `{part}` union constructor must be lowered before HTTP encode")]
    MultipartUnionConstructor { part: String },
    #[error("multipart attachment value must be an object")]
    AttachmentObjectRequired,
    #[error("multipart file part expects __plasm_attachment metadata")]
    AttachmentMetadataMissing,
    #[error("multipart attachment bytes_base64 is empty")]
    AttachmentBytesEmpty,
    #[error("multipart attachment base64 decode failed: {0}")]
    AttachmentBase64(#[from] base64::DecodeError),
    #[error("multipart file parts require bytes_base64; URI-only attachments are not sent")]
    AttachmentUriUnsupported,
    #[error("multipart attachment must include non-empty bytes_base64")]
    AttachmentBytesMissing,
    #[error("form_urlencoded body must be a flat object of scalar fields")]
    FormObjectRequired,
    #[error("form_urlencoded field `{field}` must be null or a scalar string/number/bool")]
    FormScalarRequired { field: String },
    #[error("unbound program operand reached HTTP body encoding")]
    UnboundOperand,
}
