//! Interactive Plasm REPL, available with the `baml` feature.

#[cfg(feature = "baml")]
#[path = "baml_impl.rs"]
mod baml_impl;

#[cfg(feature = "baml")]
pub use baml_impl::run_repl_main;
