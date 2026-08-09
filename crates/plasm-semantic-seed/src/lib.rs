//! Optional BAML-backed semantic seed selection.

#[cfg(feature = "baml")]
#[allow(
    clippy::derivable_impls,
    clippy::empty_line_after_doc_comments,
    clippy::map_clone,
    clippy::new_without_default,
    clippy::upper_case_acronyms,
    clippy::unwrap_or_default
)]
#[path = "../baml_client/mod.rs"]
mod baml_client;

#[cfg(feature = "baml")]
#[path = "baml_impl.rs"]
mod baml_impl;

#[cfg(feature = "baml")]
pub use baml_impl::*;
