//! MCP transport session state and telemetry counters.

use std::sync::{Arc, Mutex as StdMutex};

use super::*;
use tokio::sync::Mutex;

pub(crate) use crate::mcp_transport_store::PlasmExecBinding;

/// Cumulative MCP-side text volume for token-ish telemetry (Unicode scalar counts).
#[derive(Clone, Default, Debug)]
pub(crate) struct McpSessionPlasmStats {
    /// Plasm instructions body from `plasm_context` tool results.
    pub(crate) teaching_prompt_chars: u64,
    /// `plasm` program text plus optional `reasoning`; `plasm_run` contributes only token args.
    pub(crate) plasm_invocation_chars: u64,
    /// Successful `plasm` / `plasm_run` tool Markdown bodies.
    pub(crate) plasm_response_chars: u64,
    pub(crate) plasm_call_count: u64,
}

pub(crate) struct McpLogicalSessionState {
    pub(crate) binding: Option<PlasmExecBinding>,
    pub(crate) stats: McpSessionPlasmStats,
    pub(crate) meta_index: Arc<StdMutex<PlasmMetaIndex>>,
}

impl Default for McpLogicalSessionState {
    fn default() -> Self {
        Self {
            binding: None,
            stats: McpSessionPlasmStats::default(),
            meta_index: Arc::new(StdMutex::new(PlasmMetaIndex::new())),
        }
    }
}

#[derive(Default)]
pub(crate) struct McpTransportState {
    /// Logical session UUID string → per-agent state (execute binding, stats, `_meta.plasm` index).
    pub(crate) logical_by_id: HashMap<String, Arc<Mutex<McpLogicalSessionState>>>,
    /// Cached artifact retrieval mode for this MCP transport (see [`crate::mcp_run_markdown::ArtifactAccessMode`]).
    pub(crate) artifact_access_mode: Option<crate::mcp_run_markdown::ArtifactAccessMode>,
    /// Optional HTTP User-Agent captured for artifact-access detection (`PLASM_MCP_CLIENT_USER_AGENT` fallback).
    pub(crate) client_user_agent: Option<String>,
}

impl McpTransportState {
    pub(crate) fn to_persisted(&self) -> crate::mcp_transport_store::PersistedPlasmTransportState {
        use crate::mcp_transport_store::PersistedPlasmTransportState;
        PersistedPlasmTransportState {
            logical_bindings: HashMap::new(),
        }
    }

    pub(crate) fn with_persisted_bindings(
        mut self,
        bindings: HashMap<String, PlasmExecBinding>,
    ) -> Self {
        for (id, binding) in bindings {
            self.logical_by_id.insert(
                id,
                Arc::new(Mutex::new(McpLogicalSessionState {
                    binding: Some(binding),
                    ..Default::default()
                })),
            );
        }
        self
    }

    pub(crate) fn from_persisted(
        p: crate::mcp_transport_store::PersistedPlasmTransportState,
    ) -> Self {
        Self {
            logical_by_id: HashMap::new(),
            artifact_access_mode: None,
            client_user_agent: None,
        }
        .with_persisted_bindings(p.logical_bindings)
    }
}

/// Rough token estimate for logging (Latin-heavy text; not a billing tokenizer).
#[inline]
pub(crate) fn mcp_chars_to_token_est(chars: u64) -> u64 {
    chars.saturating_add(3) / 4
}

/// Per planning call: count program + optional reasoning for invocation telemetry.
pub(crate) fn plasm_invocation_char_count(program: &str, reasoning: Option<&str>) -> u64 {
    let mut n = program.chars().count() as u64;
    if let Some(r) = reasoning {
        n = n.saturating_add(r.chars().count() as u64);
    }
    n
}
