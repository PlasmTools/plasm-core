//! MCP `plasm` / `plasm_run` argument parsing.

use plasm_core::{PagingHandle, PlanCommitRef};
use rust_mcp_sdk::schema::{CallToolError, CallToolResult};

#[derive(Debug, Clone)]
pub(crate) enum McpPlasmRunTarget {
    Commit(PlanCommitRef),
    Page(PagingHandle),
}

#[derive(Debug, Clone)]
pub(crate) enum McpPlasmInvocation {
    Dry { program: String },
    Run(McpPlasmRunTarget),
}

impl McpPlasmInvocation {
    pub(crate) fn program(&self) -> Option<&str> {
        match self {
            Self::Dry { program } => Some(program.as_str()),
            Self::Run { .. } => None,
        }
    }

    pub(crate) fn plan_commit_ref(&self) -> Option<&PlanCommitRef> {
        match self {
            Self::Dry { .. } => None,
            Self::Run(McpPlasmRunTarget::Commit(pc)) => Some(pc),
            Self::Run(McpPlasmRunTarget::Page(_)) => None,
        }
    }

    pub(crate) fn page_handle(&self) -> Option<&PagingHandle> {
        match self {
            Self::Dry { .. } => None,
            Self::Run(McpPlasmRunTarget::Commit(_)) => None,
            Self::Run(McpPlasmRunTarget::Page(h)) => Some(h),
        }
    }

    pub(crate) fn invocation_text(&self) -> &str {
        match self {
            Self::Dry { program } => program.as_str(),
            Self::Run(McpPlasmRunTarget::Commit(pc)) => pc.as_str(),
            Self::Run(McpPlasmRunTarget::Page(h)) => h.as_str(),
        }
    }
}

fn looks_like_paging_handle_token(raw: &str) -> bool {
    let s = raw.trim();
    s.contains("_pg") || s.starts_with("page(")
}

fn parse_mcp_page_handle_param(raw: &str) -> Result<PagingHandle, String> {
    let s = raw.trim();
    let inner = s
        .strip_prefix("page(")
        .and_then(|rest| rest.strip_suffix(')'))
        .map(str::trim)
        .unwrap_or(s);
    PagingHandle::parse(inner).map_err(|e| e.to_string())
}

fn paging_handle_misuse_hint() -> &'static str {
    "Use `page_handle` on `plasm_run` with the token from the prior result's \"more pages\" line."
}

pub(crate) fn parse_mcp_plasm_invocation(
    tool_name: &'static str,
    v: &serde_json::Value,
    dry_run_only: bool,
) -> Result<McpPlasmInvocation, CallToolResult> {
    fn invalid(tool_name: &'static str, msg: impl Into<String>) -> CallToolResult {
        CallToolResult::with_error(CallToolError::invalid_arguments(
            tool_name,
            Some(msg.into()),
        ))
    }

    if dry_run_only && v.get("execute").is_some() {
        return Err(invalid(
            tool_name,
            "remove `execute`: `plasm` is plan-only. Call `plasm_run` with the same `logical_session_ref` and returned `plan_commit_ref` for live execution after reviewing the dry-run plan.",
        ));
    }

    if dry_run_only {
        let Some(program) = v
            .get("program")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            return Err(invalid(
                tool_name,
                "missing or invalid `program`: non-empty string",
            ));
        };
        if let Some(msg) = crate::operation::plasm_dry_run_continuation_error(&program) {
            return Err(invalid(tool_name, msg));
        }
        return Ok(McpPlasmInvocation::Dry { program });
    }

    for removed_key in ["program", "wait", "cancel", "force", "execute"] {
        if v.get(removed_key).is_some() {
            let msg = match removed_key {
                "program" => {
                    if v.get("program")
                        .and_then(|x| x.as_str())
                        .is_some_and(looks_like_paging_handle_token)
                    {
                        paging_handle_misuse_hint()
                    } else {
                        "`plasm_run` no longer accepts `program`; call `plasm` first, then pass only the returned `plan_commit_ref`, or `page_handle` for the next page."
                    }
                }
                "wait" => "MCP `plasm_run` always awaits server-side and does not accept `wait`.",
                "cancel" => "MCP `plasm_run` does not accept `cancel`; live runs await server-side and operation cancellation is not agent-accessible on MCP.",
                "force" => "MCP `plasm_run` does not accept `force`; execute the reviewed `plan_commit_ref` returned by `plasm`.",
                "execute" => "MCP `plasm_run` does not accept `execute`; pass `plan_commit_ref` or `page_handle`.",
                _ => unreachable!("removed key list is exhaustive"),
            };
            return Err(invalid(tool_name, msg));
        }
    }
    let plan_commit_raw = v
        .get("plan_commit_ref")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let page_handle_raw = v
        .get("page_handle")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (plan_commit_raw, page_handle_raw) {
        (Some(_), Some(_)) => Err(invalid(
            tool_name,
            "pass exactly one of `plan_commit_ref` or `page_handle`, not both",
        )),
        (None, None) => Err(invalid(
            tool_name,
            "missing `plan_commit_ref` or `page_handle`: call `plasm` first for a new run, or pass `page_handle` from a prior result's \"more pages\" line",
        )),
        (Some(raw), None) => PlanCommitRef::parse(raw)
            .map(|pc| McpPlasmInvocation::Run(McpPlasmRunTarget::Commit(pc)))
            .ok_or_else(|| {
                if looks_like_paging_handle_token(raw) {
                    invalid(tool_name, paging_handle_misuse_hint())
                } else {
                    invalid(
                        tool_name,
                        "invalid `plan_commit_ref`: expected `pcN` from a prior `plasm` dry-run",
                    )
                }
            }),
        (None, Some(raw)) => parse_mcp_page_handle_param(raw)
            .map(|handle| McpPlasmInvocation::Run(McpPlasmRunTarget::Page(handle)))
            .map_err(|e| invalid(tool_name, format!("invalid `page_handle`: {e}"))),
    }
}
