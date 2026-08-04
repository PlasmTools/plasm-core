//! Background watcher: MCP transport disconnect, op progress notify, teaching-prompt token logs.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use rust_mcp_sdk::mcp_server::HyperServer;
use rust_mcp_sdk::schema::schema_utils::CustomNotification;
use rust_mcp_sdk::McpServer;
use serde_json::json;
use tokio::sync::{Mutex, RwLock};

use crate::server_state::PlasmHostState;

use super::transport::{mcp_chars_to_token_est, McpSessionPlasmStats, McpTransportState};

/// When set and > 0, active traces with no hub activity for this many milliseconds are moved to
/// `completed` even if the MCP transport session is still in the SDK store (list UIs stop showing `live`).
fn mcp_trace_idle_finish_ms() -> u64 {
    std::env::var("PLASM_MCP_TRACE_IDLE_FINISH_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Detect MCP transport sessions that disappeared from the SDK session store (disconnect / DELETE),
/// finalize logical-session traces that are no longer live, and drop orphaned per-transport state.
#[allow(private_interfaces)]
pub(crate) fn spawn_mcp_teaching_prompt_session_reporter(
    server: &HyperServer,
    plasm: Arc<PlasmHostState>,
    session_states: Arc<RwLock<HashMap<String, Arc<Mutex<McpTransportState>>>>>,
) {
    let store = server.state().session_store.clone();
    tokio::spawn(async move {
        type SessionStates = Arc<RwLock<HashMap<String, Arc<Mutex<McpTransportState>>>>>;
        async fn stats_for_logical_session(
            session_states: &SessionStates,
            logical_id: &str,
        ) -> McpSessionPlasmStats {
            let g = session_states.read().await;
            for st in g.values() {
                let s = st.lock().await;
                if let Some(ls) = s.logical_by_id.get(logical_id) {
                    let lg = ls.lock().await;
                    return lg.stats.clone();
                }
            }
            McpSessionPlasmStats::default()
        }

        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            for pending in plasm.op_progress_hub.drain_mcp_pending() {
                let mut op_params = serde_json::Map::new();
                op_params.insert("line".into(), json!(pending.line));
                op_params.insert("n".into(), json!(pending.n));
                if let Some(c) = pending.plan_commit {
                    op_params.insert("c".into(), json!(c));
                }
                if let Some(calls) = pending.stats.calls {
                    op_params.insert("calls".into(), json!(calls));
                }
                if let Some(last_ms) = pending.stats.last_ms {
                    op_params.insert("last_ms".into(), json!(last_ms));
                }
                if let Some(elapsed_ms) = pending.stats.elapsed_ms {
                    op_params.insert("elapsed_ms".into(), json!(elapsed_ms));
                }
                if let Some(rows) = pending.stats.rows {
                    op_params.insert("rows".into(), json!(rows));
                }
                if let Some(transport) = store.get(&pending.transport_key).await {
                    let _ = transport
                        .notify_custom(CustomNotification {
                            method: "notifications/plasm/op".into(),
                            params: Some(op_params),
                        })
                        .await;
                }
            }
            let current: HashSet<String> = store.keys().await.into_iter().collect();
            let mut live_trace_keys: HashSet<String> = HashSet::new();
            {
                let g = session_states.read().await;
                for tk in &current {
                    if let Some(st_arc) = g.get(tk) {
                        let s = st_arc.lock().await;
                        for lid in s.logical_by_id.keys() {
                            live_trace_keys.insert(lid.clone());
                        }
                    }
                }
            }
            let trace_hub_active = plasm.trace_hub.active_mcp_session_count().await;
            tracing::trace!(
                target: "plasm_agent::mcp",
                session_store_keys = current.len(),
                live_logical_sessions = live_trace_keys.len(),
                trace_hub_active,
                "trace hub vs MCP session store"
            );
            let finalized = plasm
                .trace_hub
                .finalize_disconnected_sessions(&live_trace_keys)
                .await;
            for ended in &finalized {
                let stats = stats_for_logical_session(&session_states, ended).await;
                let tp = mcp_chars_to_token_est(stats.teaching_prompt_chars);
                let ti = mcp_chars_to_token_est(stats.plasm_invocation_chars);
                let tr = mcp_chars_to_token_est(stats.plasm_response_chars);
                let tt = tp.saturating_add(ti).saturating_add(tr);
                tracing::info!(
                    target: "plasm_agent::mcp",
                    logical_session_id = %ended,
                    teaching_prompt_chars_total = stats.teaching_prompt_chars,
                    plasm_invocation_chars_total = stats.plasm_invocation_chars,
                    plasm_response_chars_total = stats.plasm_response_chars,
                    plasm_call_count_total = stats.plasm_call_count,
                    tokens_est_prompt = tp,
                    tokens_est_invocation = ti,
                    tokens_est_tool_response = tr,
                    tokens_est_session_total = tt,
                    "MCP logical session trace finalized (no live transport binding)"
                );
            }
            {
                let mut g = session_states.write().await;
                g.retain(|tk, _| current.contains(tk));
            }
            let idle_ms = mcp_trace_idle_finish_ms();
            if idle_ms > 0 {
                let finalized_idle = plasm
                    .trace_hub
                    .finalize_idle_traces(&live_trace_keys, idle_ms)
                    .await;
                for ended in finalized_idle {
                    tracing::info!(
                        target: "plasm_agent::mcp",
                        logical_session_id = %ended,
                        idle_ms,
                        "MCP logical session trace finalized (idle timeout); transport still connected"
                    );
                }
            }
        }
    });
}
