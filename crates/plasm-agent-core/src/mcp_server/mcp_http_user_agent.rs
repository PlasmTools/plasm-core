//! Capture HTTP `User-Agent` on Streamable MCP requests for artifact-access detection.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request};
use axum::middleware::Next;

use crate::server_state::PlasmHostState;

const MCP_SESSION_ID_HEADER: &str = "mcp-session-id";

/// Axum middleware: stash User-Agent keyed by MCP transport session id (request or response header).
pub async fn capture_mcp_http_user_agent(
    State(plasm): State<Arc<PlasmHostState>>,
    req: Request<Body>,
    next: Next,
) -> axum::response::Response {
    let user_agent = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let session_from_req = mcp_session_id(req.headers());

    let resp = next.run(req).await;

    if let Some(ua) = user_agent {
        let session_key = session_from_req.or_else(|| mcp_session_id(resp.headers()));
        if let Some(key) = session_key {
            plasm.record_mcp_http_user_agent(&key, ua);
        }
    }
    resp
}

fn mcp_session_id(headers: &header::HeaderMap) -> Option<String> {
    headers
        .get(MCP_SESSION_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}
