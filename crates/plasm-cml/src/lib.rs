//! CML template AST, parsing, and HTTP/EVM transport compilation for Plasm.

pub mod cml;
mod credential;
pub mod error;
mod expression_validation;
mod format_template;
mod mail;
pub use format_template::FormatTemplate;
mod projection;
pub use credential::{
    CompiledCredentialBind, CompiledCredentialUse, CredentialBindTemplate, CredentialSource,
};
pub use projection::{UrlPathPart, UrlProjection};
pub mod pagination_validate;
pub mod transport;
pub(crate) mod wire_normalize;

#[cfg(feature = "evm")]
pub mod evm_transport;

pub use cml::*;
pub use error::CmlError;
pub use pagination_validate::{PaginationContractError, ValidatedPagination};
pub use transport::{
    compile_operation, parse_capability_template, template_pagination, template_var_names,
    CapabilityTemplate, CompiledOperation, ViewCompiled, ViewTemplate,
};
