//! CML template AST, parsing, and HTTP/EVM transport compilation for Plasm.

pub mod cml;
pub mod error;
pub(crate) mod gmail_send_body;
pub mod transport;

#[cfg(feature = "evm")]
pub mod evm_transport;

pub use cml::*;
pub use error::CmlError;
pub use transport::{
    compile_operation, parse_capability_template, template_pagination, template_var_names,
    CapabilityTemplate, CompiledOperation, ViewCompiled, ViewTemplate,
};
