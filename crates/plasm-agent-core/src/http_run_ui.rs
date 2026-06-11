//! HTTP routes for run explorer MCP App UI.

use axum::Router;

pub fn run_ui_routes() -> Router {
    crate::mcp_app::mount_bundle(&crate::mcp_app::RUN_EXPLORER)
}
