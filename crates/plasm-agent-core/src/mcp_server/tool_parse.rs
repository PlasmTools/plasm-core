//! Shared MCP tool argument parsing helpers.

use plasm_trace::TraceCompWire;

use crate::session_identity::PlasmContextSessionMode;

use super::*;

pub(crate) fn parse_plasm_context_session_mode(
    tool: &str,
    v: &serde_json::Value,
) -> Result<(PlasmContextSessionMode, Option<String>), CallToolError> {
    let mode_raw = v.get("session_mode").and_then(|x| x.as_str()).ok_or_else(|| {
        CallToolError::invalid_arguments(
            tool,
            Some(
                "missing `session_mode`: pass `\"new\"` to mint a session or `\"extend\"` to continue one"
                    .into(),
            ),
        )
    })?;
    let mode = PlasmContextSessionMode::parse(mode_raw).ok_or_else(|| {
        CallToolError::invalid_arguments(
            tool,
            Some(format!(
                "invalid `session_mode` `{mode_raw}`: expected \"new\" or \"extend\""
            )),
        )
    })?;
    let ref_present = v
        .get("logical_session_ref")
        .and_then(|x| x.as_str())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    match mode {
        PlasmContextSessionMode::New => {
            if ref_present.is_some() {
                return Err(CallToolError::invalid_arguments(
                    tool,
                    Some(
                        "`logical_session_ref` must not be set when `session_mode` is \"new\""
                            .into(),
                    ),
                ));
            }
            Ok((mode, None))
        }
        PlasmContextSessionMode::Extend => {
            let wire = ref_present.ok_or_else(|| {
                CallToolError::invalid_arguments(
                    tool,
                    Some(
                        "`session_mode: \"extend\"` requires `logical_session_ref` from a prior `plasm_context` call"
                            .into(),
                    ),
                )
            })?;
            let canonical = parse_logical_session_wire_ref(wire)
                .map(format_logical_session_wire_ref)
                .map_err(|e| CallToolError::invalid_arguments(tool, Some(e.to_string())))?;
            Ok((mode, Some(canonical)))
        }
    }
}

pub(crate) fn parse_optional_principal(v: &serde_json::Value) -> Option<String> {
    v.get("principal")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

pub(crate) fn parse_logical_session_ref_arg(
    tool: &str,
    v: &serde_json::Value,
) -> Result<String, CallToolError> {
    let s = v
        .get("logical_session_ref")
        .and_then(|x| x.as_str())
        .ok_or_else(|| {
            CallToolError::invalid_arguments(
                tool,
                Some("missing `logical_session_ref`: call `plasm_context` first".into()),
            )
        })?;
    parse_logical_session_wire_ref(s.trim())
        .map(format_logical_session_wire_ref)
        .map_err(|e| CallToolError::invalid_arguments(tool, Some(e.to_string())))
}

pub(crate) fn comp_content_sha256_hex(comp: &TraceCompWire) -> String {
    crate::evidence_chain::semantic_comp_commit_hex(&comp.comp)
}

pub(crate) fn plan_display_name_from_comp(comp: &TraceCompWire) -> String {
    comp.plan_display_name()
}

pub(crate) fn plan_node_count_from_comp(comp: &TraceCompWire) -> usize {
    comp.node_count()
}
