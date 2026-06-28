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

    pub(crate) fn run_target(&self) -> Option<&McpPlasmRunTarget> {
        match self {
            Self::Dry { .. } => None,
            Self::Run(target) => Some(target),
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

fn strip_page_program_wrapper(raw: &str) -> &str {
    let s = raw.trim();
    s.strip_prefix("page(")
        .and_then(|rest| rest.strip_suffix(')'))
        .map(str::trim)
        .unwrap_or(s)
}

fn parse_mcp_run_ref(raw: &str) -> Result<McpPlasmRunTarget, String> {
    let token = strip_page_program_wrapper(raw);
    if let Ok(handle) = PagingHandle::parse(token) {
        return Ok(McpPlasmRunTarget::Page(handle));
    }
    if let Some(pc) = PlanCommitRef::parse(token) {
        return Ok(McpPlasmRunTarget::Commit(pc));
    }
    Err(format!(
        "expected `pcN` from a prior `plasm` dry-run, or a page handle from a prior result's \"more pages\" line (got `{token}`)"
    ))
}

fn program_looks_like_paging_continuation(program: &str) -> bool {
    let t = strip_page_program_wrapper(program.trim());
    PagingHandle::parse(t).is_ok()
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
            "remove `execute`: `plasm` is plan-only. Call `plasm_run` with the same `logical_session_ref` and returned `run_ref` for live execution after reviewing the dry-run plan.",
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

    for removed_key in [
        "program",
        "wait",
        "cancel",
        "force",
        "execute",
        "page_handle",
        "plan_commit_ref",
    ] {
        if v.get(removed_key).is_some() {
            let msg = match removed_key {
                "program" => {
                    if v.get("program")
                        .and_then(|x| x.as_str())
                        .is_some_and(program_looks_like_paging_continuation)
                    {
                        "`plasm_run` does not accept `program`; pass the page handle as `run_ref` (not `page(...)` — that is HTTP-execute program syntax)."
                    } else {
                        "`plasm_run` no longer accepts `program`; call `plasm` first, then pass the returned `run_ref`, or a page handle from a prior result's \"more pages\" line."
                    }
                }
                "page_handle" => "`page_handle` was removed; pass the token as `run_ref` on `plasm_run`.",
                "plan_commit_ref" => "`plan_commit_ref` was removed; pass the token as `run_ref` on `plasm_run`.",
                "wait" => "MCP `plasm_run` always awaits server-side and does not accept `wait`.",
                "cancel" => "MCP `plasm_run` does not accept `cancel`; live runs await server-side and operation cancellation is not agent-accessible on MCP.",
                "force" => "MCP `plasm_run` does not accept `force`; execute the reviewed `run_ref` returned by `plasm`.",
                "execute" => "MCP `plasm_run` does not accept `execute`; pass `run_ref`.",
                _ => unreachable!("removed key list is exhaustive"),
            };
            return Err(invalid(tool_name, msg));
        }
    }
    let run_ref_raw = v
        .get("run_ref")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match run_ref_raw {
        None => Err(invalid(
            tool_name,
            "missing `run_ref`: call `plasm` first for a new run, or pass the page handle from a prior result's \"more pages\" line",
        )),
        Some(raw) => parse_mcp_run_ref(raw)
            .map(McpPlasmInvocation::Run)
            .map_err(|e| invalid(tool_name, format!("invalid `run_ref`: {e}"))),
    }
}
