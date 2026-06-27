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

    for removed_key in ["program", "wait", "cancel", "force", "execute", "page_handle"] {
        if v.get(removed_key).is_some() {
            let msg = match removed_key {
                "program" => {
                    if v.get("program")
                        .and_then(|x| x.as_str())
                        .is_some_and(looks_like_paging_handle_token)
                    {
                        "`plasm_run` does not accept `program`; pass the page handle from the prior result's \"more pages\" line as `plan_commit_ref` (not `page(...)` — that is HTTP-execute program syntax)."
                    } else {
                        "`plasm_run` no longer accepts `program`; call `plasm` first, then pass the returned `plan_commit_ref`, or the page handle from a prior result's \"more pages\" line."
                    }
                }
                "page_handle" => "`page_handle` was removed; pass the page handle as `plan_commit_ref` on `plasm_run`.",
                "wait" => "MCP `plasm_run` always awaits server-side and does not accept `wait`.",
                "cancel" => "MCP `plasm_run` does not accept `cancel`; live runs await server-side and operation cancellation is not agent-accessible on MCP.",
                "force" => "MCP `plasm_run` does not accept `force`; execute the reviewed `plan_commit_ref` returned by `plasm`.",
                "execute" => "MCP `plasm_run` does not accept `execute`; pass `plan_commit_ref`.",
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
    match plan_commit_raw {
        None => Err(invalid(
            tool_name,
            "missing `plan_commit_ref`: call `plasm` first for a new run, or pass the page handle from a prior result's \"more pages\" line",
        )),
        Some(raw) if looks_like_paging_handle_token(raw) => parse_mcp_page_handle_param(raw)
            .map(|handle| McpPlasmInvocation::Run(McpPlasmRunTarget::Page(handle)))
            .map_err(|e| invalid(tool_name, format!("invalid page handle in `plan_commit_ref`: {e}"))),
        Some(raw) => PlanCommitRef::parse(raw)
            .map(|pc| McpPlasmInvocation::Run(McpPlasmRunTarget::Commit(pc)))
            .ok_or_else(|| {
                invalid(
                    tool_name,
                    "invalid `plan_commit_ref`: expected `pcN` from a prior `plasm` dry-run, or a page handle from a prior result's \"more pages\" line",
                )
            }),
    }
}
