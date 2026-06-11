//! HTTP routes for general plan review MCP App UI.

use axum::Router;

pub fn plan_ui_routes() -> Router {
    crate::mcp_app::mount_bundle(&crate::mcp_app::PLAN_REVIEW)
}
