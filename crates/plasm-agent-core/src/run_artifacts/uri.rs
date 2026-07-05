use super::types::RunArtifactId;
use crate::mcp_logical_ref::{
    format_logical_session_wire_ref_from_uuid, parse_logical_session_wire_ref,
};
use plasm_trace::MCP_RESOURCE_READ_SOURCE_QUERY_KEY;
use uuid::Uuid;

pub fn plasm_run_resource_uri(
    prompt_hash: &str,
    session_id: &str,
    run_id: &RunArtifactId,
) -> String {
    format!(
        "plasm://execute/{prompt_hash}/{session_id}/run/{}",
        run_id.to_wire()
    )
}

/// Short LLM-facing URI; resolve via MCP `resources/read` using the bound execute session (HTTP / legacy).
pub fn plasm_short_resource_uri(resource_index: u64) -> String {
    format!("plasm://r/{resource_index}")
}

/// Short URI scoped to an MCP **logical session** (agent identity), not transport.
/// `session_segment` is the canonical wire ref (`l_<token>`).
///
/// **Deprecated for new emits:** index-only URIs are ambiguous across execute-session rebinding
/// and vs plan `run_step`. Prefer [`plasm_session_short_run_uri`].
pub fn plasm_session_short_resource_uri(session_segment: &str, resource_index: u64) -> String {
    format!("plasm://session/{session_segment}/r/{resource_index}")
}

/// Content-addressed short URI for MCP logical sessions (`plasm://session/l_<token>/run/pr…`).
pub fn plasm_session_short_run_uri(session_segment: &str, run_id: &RunArtifactId) -> String {
    format!("plasm://session/{session_segment}/run/{}", run_id.to_wire())
}

pub fn code_plan_handle(plan_index: u64) -> String {
    format!("p{plan_index}")
}

pub fn parse_code_plan_handle(handle: &str) -> Option<u64> {
    let rest = handle.strip_prefix('p')?;
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// Short LLM-facing program-plan URI; resolve via bound execute session.
pub fn plasm_short_code_plan_uri(plan_index: u64) -> String {
    format!("plasm://p/{plan_index}")
}

/// Short program-plan URI scoped to an MCP logical session wire ref.
pub fn plasm_session_short_plan_uri(session_segment: &str, plan_index: u64) -> String {
    format!("plasm://session/{session_segment}/p/{plan_index}")
}

/// Canonical URI for a permanent program plan archive document.
pub fn plasm_code_plan_resource_uri(prompt_hash: &str, session_id: &str, plan_id: &Uuid) -> String {
    format!("plasm://execute/{prompt_hash}/{session_id}/plan/{plan_id}")
}

/// Short logical-session URI from canonical UUID (formats `l_<token>`).
pub fn plasm_short_resource_uri_logical(logical_session_id: &Uuid, resource_index: u64) -> String {
    plasm_session_short_resource_uri(
        &format_logical_session_wire_ref_from_uuid(*logical_session_id),
        resource_index,
    )
}

/// Short logical-session run URI from canonical UUID (formats `l_<token>`).
pub fn plasm_short_run_uri_logical(logical_session_id: &Uuid, run_id: &RunArtifactId) -> String {
    plasm_session_short_run_uri(
        &format_logical_session_wire_ref_from_uuid(*logical_session_id),
        run_id,
    )
}

/// Parse `plasm://session/l_<token>/run/{run_id}` (content-addressed short URI).
pub fn parse_plasm_session_short_run_uri(
    uri: &str,
) -> Option<(LogicalSessionUriSegment, RunArtifactId)> {
    let rest = uri.strip_prefix("plasm://session/")?;
    let mut parts = rest.split('/').filter(|s| !s.is_empty());
    let seg = parts.next()?;
    let segment = parse_session_segment(seg)?;
    let run = parts.next()?;
    let wire = parts.next()?;
    if run != "run" {
        return None;
    }
    let run_id = RunArtifactId::from_wire(wire)?;
    if parts.next().is_some() {
        return None;
    }
    Some((segment, run_id))
}

/// First path segment after `plasm://session/` for short run resources: canonical `l_<token>` only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogicalSessionUriSegment {
    WireRef(String),
}

fn parse_session_segment(seg: &str) -> Option<LogicalSessionUriSegment> {
    parse_logical_session_wire_ref(seg)
        .ok()
        .map(|_| LogicalSessionUriSegment::WireRef(seg.to_string()))
}

/// Parse `plasm://r/{decimal}` (no extra path segments).
pub fn parse_plasm_short_resource_uri(uri: &str) -> Option<u64> {
    let rest = uri.strip_prefix("plasm://r/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    if !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    rest.parse().ok()
}

/// Parse `plasm://session/l_<token>/r/{decimal}`.
pub fn parse_plasm_session_short_resource_uri(
    uri: &str,
) -> Option<(LogicalSessionUriSegment, u64)> {
    let rest = uri.strip_prefix("plasm://session/")?;
    let mut parts = rest.split('/').filter(|s| !s.is_empty());
    let seg = parts.next()?;
    let segment = parse_session_segment(seg)?;
    let r = parts.next()?;
    let n = parts.next()?;
    if r != "r" {
        return None;
    }
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if parts.next().is_some() {
        return None;
    }
    let idx: u64 = n.parse().ok()?;
    Some((segment, idx))
}

/// Parse `plasm://session/l_<token>/p/{decimal}`.
pub fn parse_plasm_session_short_plan_uri(uri: &str) -> Option<(LogicalSessionUriSegment, u64)> {
    let rest = uri.strip_prefix("plasm://session/")?;
    let mut parts = rest.split('/').filter(|s| !s.is_empty());
    let seg = parts.next()?;
    let segment = parse_session_segment(seg)?;
    let p = parts.next()?;
    let n = parts.next()?;
    if p != "p" {
        return None;
    }
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if parts.next().is_some() {
        return None;
    }
    let idx: u64 = n.parse().ok()?;
    Some((segment, idx))
}

pub fn logical_uuid_from_uri_segment(segment: &LogicalSessionUriSegment) -> Option<Uuid> {
    match segment {
        LogicalSessionUriSegment::WireRef(wire) => parse_logical_session_wire_ref(wire)
            .ok()
            .map(|id| id.as_uuid()),
    }
}

pub fn artifact_http_path(prompt_hash: &str, session_id: &str, run_id: &RunArtifactId) -> String {
    format!(
        "/execute/{prompt_hash}/{session_id}/artifacts/{}",
        run_id.to_wire()
    )
}

pub fn code_plan_http_path(prompt_hash: &str, session_id: &str, plan_id: &Uuid) -> String {
    format!("/execute/{prompt_hash}/{session_id}/plans/{plan_id}")
}

/// Strip optional `plasm.read_source=` query hint from MCP resource URIs before resolution.
pub fn strip_plasm_resource_read_source(uri: &str) -> (String, Option<String>) {
    let Some((base, query)) = uri.split_once('?') else {
        return (uri.to_string(), None);
    };
    let mut read_source = None;
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            if let Some(v) = pair.strip_prefix(&format!("{MCP_RESOURCE_READ_SOURCE_QUERY_KEY}=")) {
                if !v.is_empty() {
                    read_source = Some(v.to_string());
                }
                return false;
            }
            true
        })
        .collect();
    if kept.is_empty() {
        (base.to_string(), read_source)
    } else {
        (format!("{}?{}", base, kept.join("&")), read_source)
    }
}

/// Parse `plasm://execute/{prompt_hash}/{session_id}/run/{run_id}` (`run_id` = prefixed hex digest).
pub fn parse_plasm_execute_run_uri(uri: &str) -> Option<(String, String, RunArtifactId)> {
    let rest = uri.strip_prefix("plasm://execute/")?;
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() != 4 || parts[2] != "run" {
        return None;
    }
    let run_id = RunArtifactId::from_wire(parts[3])?;
    Some((parts[0].to_string(), parts[1].to_string(), run_id))
}

/// Parse `plasm://execute/{prompt_hash}/{session_id}/plan/{plan_id}`.
pub fn parse_plasm_execute_plan_uri(uri: &str) -> Option<(String, String, Uuid)> {
    let rest = uri.strip_prefix("plasm://execute/")?;
    let parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() != 4 || parts[2] != "plan" {
        return None;
    }
    let plan_id = Uuid::parse_str(parts[3]).ok()?;
    Some((parts[0].to_string(), parts[1].to_string(), plan_id))
}
