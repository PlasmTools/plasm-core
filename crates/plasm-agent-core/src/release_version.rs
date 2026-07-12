//! Release semver exposed on `/v1/health` and MCP `initialize` (stamped in `build.rs`).

/// Monorepo / image-tag release version (preferred over per-crate `CARGO_PKG_VERSION`).
pub const RELEASE_VERSION: &str = env!("PLASM_RELEASE_VERSION");
