//! Loopback-only benchmark host with explicit, task-independent catalogue exposure.
#![recursion_limit = "256"]
use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use plasm_agent_core::{
    http::{build_plasm_host_state, PlasmHostBootstrap},
    http_execute::{apply_capability_seeds, CapabilitySeed},
    intent_provenance::IntentProvenance,
    mcp_server::{build_mcp_router_for_transport, McpHttpTransport},
    server_state::{CatalogBootstrap, PlasmHostState},
};
use plasm_runtime::{
    mcp_execution::McpBindings,
    mcp_transport::{McpHttpConfig, McpSession},
    ExecutionConfig, ExecutionEngine, ExecutionMode,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostConfig {
    catalogs: Vec<CatalogBinding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogBinding {
    entry_id: String,
    directory: PathBuf,
    server: String,
    url: String,
}

#[derive(Clone)]
struct BenchmarkState {
    host: Arc<PlasmHostState>,
    seeds: Vec<(String, String)>,
}
#[derive(Deserialize)]
struct OpenSession {
    intent: String,
}
#[derive(Serialize)]
struct OpenedSession {
    logical_session_ref: String,
    prompt: String,
}
async fn open_session(
    State(state): State<BenchmarkState>,
    Json(request): Json<OpenSession>,
) -> Result<Json<OpenedSession>, (StatusCode, String)> {
    let logical = uuid::Uuid::new_v4();
    let provenance = IntentProvenance::from_turns([request.intent.clone()])
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let opened = apply_capability_seeds(
        &state.host,
        None,
        None,
        state
            .seeds
            .iter()
            .map(|(entry_id, entity)| CapabilitySeed {
                entry_id: entry_id.clone(),
                entity: entity.clone(),
            })
            .collect(),
        None,
        None,
        Some(logical),
        &request.intent,
    )
    .await
    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    state
        .host
        .logical_sessions
        .register_routed_session(
            plasm_agent_core::session_identity::LogicalSessionId(logical),
            "",
            &provenance,
            None,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state
        .host
        .logical_execute_bindings
        .insert(
            logical,
            opened.prompt_hash.clone(),
            opened.session_id.clone(),
        )
        .await;
    let session = state
        .host
        .get_execute_session(&opened.prompt_hash, &opened.session_id)
        .await
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Opened session unavailable".into(),
            )
        })?;
    Ok(Json(OpenedSession {
        logical_session_ref:
            plasm_agent_core::mcp_logical_ref::format_logical_session_wire_ref_from_uuid(logical),
        prompt: session.prompt_text.clone(),
    }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Keep stdout exclusively for the readiness handshake. Pool diagnostics are
    // operator evidence, captured by the harness in each attempt's host.log.
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(std::io::Error::other)?;
    let config_path = std::env::args()
        .nth(1)
        .ok_or("Expected host configuration JSON path")?;
    let config: HostConfig = serde_json::from_slice(&std::fs::read(config_path)?)?;
    if config.catalogs.is_empty() {
        return Err("At least one explicitly authorized catalogue is required".into());
    }
    let mut entry_ids = HashSet::new();
    let mut server_names = HashSet::new();
    for binding in &config.catalogs {
        if !entry_ids.insert(&binding.entry_id) || !server_names.insert(&binding.server) {
            return Err("Duplicate catalogue or server binding".into());
        }
    }
    let mut entries = Vec::new();
    let mut bindings = HashMap::new();
    let mut seeds = Vec::new();
    for binding in config.catalogs {
        let entry = binding.entry_id;
        let cgs = Arc::new(plasm_core::loader::load_schema_dir(&binding.directory)?);
        for entity in cgs.entities.keys() {
            seeds.push((entry.to_owned(), entity.to_string()));
        }
        entries.push((entry.clone(), entry, vec![], cgs));
        bindings.insert(
            binding.server,
            McpSession::connect_http(McpHttpConfig {
                url: binding.url.parse()?,
                headers: HashMap::new(),
                deadline: Duration::from_secs(60),
            })
            .await?,
        );
    }
    let engine = ExecutionEngine::new(ExecutionConfig {
        hydrate: false,
        ..Default::default()
    })?
    .with_mcp_bindings(McpBindings::new(bindings));
    let mut host = build_plasm_host_state(PlasmHostBootstrap {
        engine,
        mode: ExecutionMode::Live,
        registry: Arc::new(plasm_core::discovery::CgsRegistry::from_pairs(entries)),
        catalog_bootstrap: CatalogBootstrap::Fixed,
        incoming_auth: None,
        run_artifacts: Arc::new(plasm_agent_core::run_artifacts::RunArtifactStore::memory()),
        session_graph_persistence: None,
        oss_local_filesystem_defaults: false,
    })?;
    host.oss.live_plan_pool =
        Arc::new(plasm_agent_core::live_plan_run_worker::LivePlanRunPool::new());
    let host = Arc::new(host);
    let mcp =
        build_mcp_router_for_transport(host.clone(), McpHttpTransport::StreamableHttp).await?;
    let router = Router::new()
        .route("/session", post(open_session))
        .with_state(BenchmarkState { host, seeds })
        .merge(mcp);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!(
        "{}",
        serde_json::json!({"base_url": format!("http://{}", listener.local_addr()?)})
    );
    axum::serve(listener, router).await?;
    Ok(())
}
