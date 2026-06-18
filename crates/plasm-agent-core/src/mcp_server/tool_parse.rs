//! Shared MCP tool argument parsing helpers.

use plasm_trace::TraceCompWire;

use super::*;

pub(crate) fn parse_tool_seeds(
    tool: &str,
    v: &serde_json::Value,
) -> Result<Vec<CapabilitySeed>, CallToolError> {
    if v.get("seeds").is_none() && (v.get("entry_id").is_some() || v.get("entities").is_some()) {
        return Err(CallToolError::invalid_arguments(
            tool,
            Some(
                "missing capability picks: `plasm_context` requires a non-empty `seeds` array of `{api, entity}` objects (`entry_id` per object is accepted instead of `api`); old top-level `{entry_id, entities}` is not supported"
                    .into(),
            ),
        ));
    }
    let seeds: Vec<CapabilitySeed> = serde_json::from_value(
        v.get("seeds")
            .cloned()
            .ok_or_else(|| {
                CallToolError::invalid_arguments(
                    tool,
                    Some(
                        "missing capability picks: expected non-empty `seeds` array of `{api, entity}` objects (`entry_id` key accepted per object)".into(),
                    ),
                )
            })?,
    )
    .map_err(|e| CallToolError::invalid_arguments(tool, Some(e.to_string())))?;
    let seeds = normalize_capability_seeds(seeds);
    if seeds.is_empty() {
        return Err(CallToolError::invalid_arguments(
            tool,
            Some("`seeds` must be a non-empty array of capability picks: `{api, entity}` per object (`entry_id` key accepted per object)".into()),
        ));
    }
    Ok(seeds)
}

pub(crate) fn parse_plasm_context_ranked_capabilities(
    tool: &str,
    v: &serde_json::Value,
) -> Result<RankedCapabilitiesArg, CallToolError> {
    match v.get("ranked_capabilities") {
        None => Ok(RankedCapabilitiesArg::Unspecified),
        Some(serde_json::Value::Null) => Ok(RankedCapabilitiesArg::Set(None)),
        Some(serde_json::Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let s = item.as_str().ok_or_else(|| {
                    CallToolError::invalid_arguments(
                        tool,
                        Some(format!("ranked_capabilities[{i}] must be a string")),
                    )
                })?;
                out.push(s.to_string());
            }
            Ok(RankedCapabilitiesArg::Set(Some(out)))
        }
        Some(_) => Err(CallToolError::invalid_arguments(
            tool,
            Some("`ranked_capabilities` must be null or an array of strings".into()),
        )),
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
