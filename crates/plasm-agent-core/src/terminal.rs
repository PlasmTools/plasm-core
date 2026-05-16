//! Remote HTTP terminal for `plasm`: discovery, client-owned context symbols, plan/run.
//!
//! See `docs/plasm-cgs-remote-terminal.md` in the parent repo.

use anyhow::{anyhow, Context as _, Result};
use clap::{Parser, Subcommand};
use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE,
};
use reqwest::redirect::Policy;
use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::http_discovery::TerminalDiscoverBody;
use crate::http_execute::{
    build_capability_exposure_plan, CapabilitySeed, CreateExecuteSessionBody,
    CreateExecuteSessionResponse, ExecuteSessionContextBody,
};
use crate::plasm_plan::parse_and_validate_plan_json;
use crate::resolved_plan_http::{
    ResolvedPlanProtocolVersion, ResolvedPlanRequest, ResolvedPlanRunMode,
    RESOLVED_PLAN_CONTENT_TYPE,
};
use crate::terminal_session::ClientSymbolSession;
use crate::terminal_state::{
    format_qualified_capabilities, merge_and_write_latest_discovery, mint_client_session_id,
    read_current_session_pointer, resolve_capability_seeds, resolve_current_session,
    write_current_session_pointer, write_language_frontmatter_markdown, write_session_file,
    ExecutionBinding,
};

/// Default HTTP origin written by `plasm init` when `--server` is omitted.
pub const DEFAULT_PLASM_HTTP_ORIGIN: &str = "http://127.0.0.1:3000";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TerminalProfile {
    pub server: Option<String>,
    pub api_key: Option<String>,
    pub bearer_token: Option<String>,
}

/// Auth view for HTTP helpers shared with [`crate::terminal_session`].
pub struct TerminalProfileRef<'a> {
    inner: &'a TerminalProfile,
}

impl<'a> TerminalProfileRef<'a> {
    pub fn new(inner: &'a TerminalProfile) -> Self {
        Self { inner }
    }

    pub fn apply_auth_headers(&self, headers: &mut HeaderMap) -> Result<()> {
        apply_auth_headers(headers, self.inner)
    }
}

#[derive(Parser)]
#[command(
    name = "plasm",
    version = env!("CARGO_PKG_VERSION"),
    about = "Remote Plasm terminal — search, context, run (HTTP). Run `plasm init` once, then `doctor` if needed.",
)]
struct Cli {
    #[arg(long, global = true, default_value = "default")]
    profile: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Init {
        #[arg(long)]
        server: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        bearer_token: Option<String>,
    },
    Doctor,
    Search {
        #[arg(required = true)]
        intent: String,
        #[arg(long)]
        limit: Option<usize>,
    },
    Context {
        #[arg(long)]
        new: bool,
        #[arg(long)]
        verbose: bool,
        intent: Option<String>,
        capabilities: Vec<String>,
    },
    Run {
        #[arg(long, default_value = "run")]
        mode: String,
        #[arg(long, default_value = "text/plain")]
        accept: String,
        #[arg(long)]
        file: Option<PathBuf>,
    },
}

fn profile_path(name: &str) -> PathBuf {
    crate::terminal_state::home_dir()
        .join(".plasm/cgs/profiles")
        .join(format!("{name}.json"))
}

fn load_profile(name: &str) -> Result<TerminalProfile> {
    let p = profile_path(name);
    if !p.exists() {
        return Ok(TerminalProfile::default());
    }
    let raw =
        std::fs::read_to_string(&p).with_context(|| format!("read profile {}", p.display()))?;
    Ok(serde_json::from_str(&raw).unwrap_or_default())
}

fn save_profile(name: &str, prof: &TerminalProfile) -> Result<()> {
    let p = profile_path(name);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(prof)?)?;
    Ok(())
}

fn normalize_http_origin(s: &str) -> String {
    s.trim().trim_end_matches('/').to_string()
}

fn require_configured_server(profile: &TerminalProfile) -> Result<String> {
    profile
        .server
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(normalize_http_origin)
        .ok_or_else(|| {
            anyhow!(
                "Plasm is not configured. Run `plasm init` first (e.g. `plasm init --server http://127.0.0.1:3000`)."
            )
        })
}

fn resolve_api_key(profile: &TerminalProfile) -> Option<String> {
    profile
        .api_key
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn resolve_bearer(profile: &TerminalProfile) -> Option<String> {
    profile
        .bearer_token
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn apply_auth_headers(headers: &mut HeaderMap, profile: &TerminalProfile) -> Result<()> {
    let api_key = resolve_api_key(profile);
    let bearer = resolve_bearer(profile);
    match (api_key, bearer) {
        (Some(k), _) => {
            headers.insert(
                "x-api-key",
                HeaderValue::from_str(k.trim())
                    .map_err(|e| anyhow!("invalid API key header: {e}"))?,
            );
        }
        (None, Some(tok)) => {
            let v = format!("Bearer {}", tok.trim());
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&v).map_err(|e| anyhow!("invalid bearer header: {e}"))?,
            );
        }
        (None, None) => {}
    }
    Ok(())
}

fn run_init(
    profile_name: &str,
    profile: &mut TerminalProfile,
    server: Option<String>,
    api_key: Option<String>,
    bearer_token: Option<String>,
) -> Result<()> {
    if let Some(s) = server {
        let t = s.trim();
        if t.is_empty() {
            return Err(anyhow!("init: --server must not be empty"));
        }
        profile.server = Some(normalize_http_origin(t));
    } else if profile
        .server
        .as_ref()
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
    {
        profile.server = Some(DEFAULT_PLASM_HTTP_ORIGIN.to_string());
    }
    if let Some(k) = api_key {
        profile.api_key = Some(k);
    }
    if let Some(b) = bearer_token {
        profile.bearer_token = Some(b);
    }
    let path = profile_path(profile_name);
    save_profile(profile_name, profile)?;
    let grammar_path = write_language_frontmatter_markdown(
        &plasm_core::prompt_render::render_plasm_mcp_language_frontmatter(),
    )?;
    println!("configured {}", path.display());
    println!("  grammar: {}", grammar_path.display());
    println!(
        "  server: {}",
        profile.server.as_deref().unwrap_or("(none)")
    );
    println!(
        "  api_key: {}",
        if profile.api_key.as_ref().is_some_and(|s| !s.is_empty()) {
            "set"
        } else {
            "unset"
        }
    );
    println!(
        "  bearer_token: {}",
        if profile.bearer_token.as_ref().is_some_and(|s| !s.is_empty()) {
            "set"
        } else {
            "unset"
        }
    );
    Ok(())
}

fn read_program_body(path: Option<&PathBuf>) -> Result<Vec<u8>> {
    if let Some(p) = path {
        std::fs::read(p).with_context(|| format!("read {}", p.display()))
    } else {
        let mut buf = Vec::new();
        io::stdin().read_to_end(&mut buf)?;
        Ok(buf)
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_bytes(
    client: &Client,
    server: &str,
    profile: &TerminalProfile,
    method: Method,
    path: &str,
    accept: Option<&str>,
    content_type: Option<&str>,
    body: Option<Vec<u8>>,
) -> Result<(StatusCode, HeaderMap, Vec<u8>)> {
    let url = format!(
        "{}/{}",
        server.trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let mut req = client.request(method, url);
    let mut headers = HeaderMap::new();
    apply_auth_headers(&mut headers, profile)?;
    if let Some(a) = accept {
        headers.insert(ACCEPT, HeaderValue::from_str(a)?);
    }
    if let Some(ct) = content_type {
        headers.insert(CONTENT_TYPE, HeaderValue::from_str(ct)?);
    }
    if let Some(ref b) = body {
        headers.insert(CONTENT_LENGTH, HeaderValue::from(b.len()));
        req = req.headers(headers).body(b.clone());
    } else {
        req = req.headers(headers);
    }
    let res = req.send().await?;
    let status = res.status();
    let hdrs = res.headers().clone();
    let bytes = res.bytes().await?.to_vec();
    Ok((status, hdrs, bytes))
}

async fn http_create_session(
    client: &Client,
    server: &str,
    profile: &TerminalProfile,
    entry_id: &str,
    entities: Vec<String>,
    intent: Option<String>,
) -> Result<CreateExecuteSessionResponse> {
    let body_json = serde_json::to_vec(&CreateExecuteSessionBody {
        entry_id: entry_id.to_string(),
        entities,
        principal: None,
        logical_session_id: None,
        context_intent: intent,
        ranked_capabilities: None,
    })?;
    let create_client = Client::builder()
        .redirect(Policy::none())
        .build()
        .map_err(|e| anyhow!("http client (no redirect): {e}"))?;
    let url = format!("{}/execute", server.trim_end_matches('/'));
    let mut headers = HeaderMap::new();
    apply_auth_headers(&mut headers, profile)?;
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let res = create_client
        .post(url)
        .headers(headers)
        .body(body_json)
        .send()
        .await?;
    let st = res.status();
    if st != StatusCode::SEE_OTHER {
        let b = res.bytes().await?;
        return Err(anyhow!(
            "open session: expected 303, got {st}: {}",
            String::from_utf8_lossy(&b)
        ));
    }
    let loc = res
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| anyhow!("open session: missing Location header"))?;
    let session_url = if loc.starts_with("http") {
        loc.to_string()
    } else {
        format!("{}{}", server.trim_end_matches('/'), loc)
    };
    let mut gh = HeaderMap::new();
    apply_auth_headers(&mut gh, profile)?;
    gh.insert(ACCEPT, HeaderValue::from_static("application/json"));
    let get = client.get(&session_url).headers(gh).send().await?;
    let gst = get.status();
    let session_body = get.bytes().await?.to_vec();
    if !gst.is_success() {
        return Err(anyhow!(
            "open session: GET failed {gst}: {}",
            String::from_utf8_lossy(&session_body)
        ));
    }
    serde_json::from_slice(&session_body).map_err(|e| anyhow!("open session: invalid JSON: {e}"))
}

async fn http_post_context(
    client: &Client,
    server: &str,
    profile: &TerminalProfile,
    prompt_hash: &str,
    session: &str,
    intent: Option<String>,
    seeds: Vec<CapabilitySeed>,
) -> Result<()> {
    let payload = serde_json::to_vec(&ExecuteSessionContextBody { intent, seeds })?;
    let path = format!("/execute/{prompt_hash}/{session}/context");
    let (st, _, body) = send_bytes(
        client,
        server,
        profile,
        Method::POST,
        &path,
        Some("application/json"),
        Some("application/json"),
        Some(payload),
    )
    .await?;
    if !st.is_success() {
        return Err(anyhow!(
            "context: HTTP {st}: {}",
            String::from_utf8_lossy(&body)
        ));
    }
    Ok(())
}

fn context_seeds_after_open(
    seeds: &[CapabilitySeed],
    primary_entry_id: &str,
) -> Vec<CapabilitySeed> {
    seeds
        .iter()
        .filter(|s| s.entry_id != primary_entry_id)
        .cloned()
        .collect()
}

/// Lazy server execute binding for HTTP run/plan (opaque; symbols stay on the client).
async fn ensure_execution_binding(
    client: &Client,
    server: &str,
    profile: &TerminalProfile,
    sym: &mut ClientSymbolSession,
) -> Result<ExecutionBinding> {
    if let Some(ex) = sym.execution.clone() {
        return Ok(ex);
    }
    let seeds: Vec<CapabilitySeed> = sym
        .capabilities
        .iter()
        .map(|(api, entity)| CapabilitySeed {
            entry_id: api.clone(),
            entity: entity.clone(),
        })
        .collect();
    let plan = build_capability_exposure_plan(&seeds)
        .ok_or_else(|| anyhow!("empty capability set for execution binding"))?;
    let primary_api = plan.primary_entry_id.clone();
    let primary_entities = plan
        .seeds_by_entry
        .get(&primary_api)
        .cloned()
        .ok_or_else(|| anyhow!("missing entities for execution binding"))?;
    let created = http_create_session(
        client,
        server,
        profile,
        &primary_api,
        primary_entities,
        Some(sym.intent.clone()),
    )
    .await?;
    let ph = created.prompt_hash.clone();
    let sid = created.session.clone();
    let follow_on = context_seeds_after_open(&seeds, &primary_api);
    if !follow_on.is_empty() {
        http_post_context(
            client,
            server,
            profile,
            &ph,
            &sid,
            Some(sym.intent.clone()),
            follow_on,
        )
        .await?;
    }
    let binding = ExecutionBinding {
        prompt_hash: ph,
        session: sid,
    };
    sym.execution = Some(binding.clone());
    sym.persist(server)?;
    Ok(binding)
}

fn print_context_summary(capabilities: &[(String, String)], mirror: &Path, rows_added: usize) {
    println!(
        "Active context: {}",
        format_qualified_capabilities(capabilities)
    );
    if rows_added > 0 {
        eprintln!("mirror: {} (+{rows_added} rows)", mirror.display());
    } else {
        eprintln!("mirror: {}", mirror.display());
    }
}

async fn run_context_command(
    client: &Client,
    server: &str,
    profile: &TerminalProfile,
    new_session: bool,
    verbose: bool,
    intent_arg: Option<String>,
    capability_names: Vec<String>,
) -> Result<()> {
    let discovery = crate::terminal_state::read_latest_discovery(server)?;
    let seeds = resolve_capability_seeds(&capability_names, discovery.as_ref())?;

    let resolved_intent = intent_arg
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            discovery
                .as_ref()
                .and_then(|d| d.intent.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        });

    let mut sym = if new_session {
        let id = mint_client_session_id();
        let intent = resolved_intent.clone().ok_or_else(|| {
            anyhow!(
                "context: pass an intent string as the first argument, or run `plasm search` first"
            )
        })?;
        ClientSymbolSession::new(id, intent)
    } else if let Some(id) = read_current_session_pointer(server)? {
        ClientSymbolSession::load_from_disk(server, &id)?
    } else {
        let id = mint_client_session_id();
        let intent = resolved_intent.clone().ok_or_else(|| {
            anyhow!(
                "context: pass an intent string as the first argument, or run `plasm search` first"
            )
        })?;
        ClientSymbolSession::new(id, intent)
    };

    if let Some(intent) = resolved_intent {
        sym.intent = intent;
    }

    let prof_ref = TerminalProfileRef::new(profile);
    for api in seeds
        .iter()
        .map(|s| s.entry_id.as_str())
        .collect::<std::collections::HashSet<_>>()
    {
        sym.ensure_catalog(client, server, &prof_ref, api).await?;
    }

    let tsv_delta = sym.expose_seeds(&seeds)?;
    let (mirror_path, rows_added) = if tsv_delta.is_empty() {
        (
            crate::terminal_state::domain_tsv_path(server, &sym.client_session_id),
            0,
        )
    } else {
        sym.append_rendered_tsv(server, &tsv_delta)?
    };

    if verbose && !tsv_delta.is_empty() {
        println!("\n--- client exposure ---\n{tsv_delta}");
    }

    sym.persist(server)?;
    write_current_session_pointer(server, &sym.client_session_id)?;

    print_context_summary(&sym.capabilities, &mirror_path, rows_added);
    Ok(())
}

fn extract_run_id_from_response(headers: &HeaderMap, body: &[u8]) -> Option<String> {
    if let Some(rid) = headers.get("x-plasm-run-id").and_then(|v| v.to_str().ok()) {
        return Some(rid.to_string());
    }
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    v.get("_meta")
        .and_then(|m| m.get("plasm"))
        .and_then(|p| p.get("steps"))
        .and_then(|s| s.as_array())
        .and_then(|arr| arr.first())
        .and_then(|step| step.get("run_id"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

async fn mirror_run_snapshot(
    client: &Client,
    server: &str,
    profile: &TerminalProfile,
    client_session_id: &str,
    prompt_hash: &str,
    session: &str,
    run_id: &str,
) -> Result<PathBuf> {
    let path = format!("/execute/{prompt_hash}/{session}/artifacts/{run_id}");
    let (st, _, body) = send_bytes(
        client,
        server,
        profile,
        Method::GET,
        &path,
        Some("application/json"),
        None,
        None,
    )
    .await?;
    if !st.is_success() {
        return Err(anyhow!("artifact GET failed HTTP {st}"));
    }
    let run_dir = crate::terminal_state::client_session_dir(server, client_session_id)
        .join("runs")
        .join(run_id);
    std::fs::create_dir_all(&run_dir)?;
    let snap = run_dir.join("snapshot.txt");
    let text = serde_json::to_string_pretty(&serde_json::from_slice::<serde_json::Value>(&body)?)
        .unwrap_or_else(|_| String::from_utf8_lossy(&body).into_owned());
    std::fs::write(&snap, &text)?;
    let latest =
        crate::terminal_state::client_session_dir(server, client_session_id).join("latest_run.txt");
    std::fs::write(latest, format!("runs/{run_id}/snapshot.txt\n"))?;
    Ok(snap)
}

async fn run_doctor(profile_name: &str, profile: &TerminalProfile) -> Result<()> {
    let p = profile_path(profile_name);
    println!("plasm remote — diagnostics");
    println!();
    println!("Profile: {}", p.display());
    println!("  exists: {}", p.exists());
    println!();
    let origin = match require_configured_server(profile) {
        Ok(o) => o,
        Err(e) => {
            println!("HTTP API origin: (not configured)");
            println!("  {e}");
            println!();
            println!("Agent flow: `plasm init` → `search` → `context \"intent\" Cap …` → `run`");
            return Ok(());
        }
    };
    println!("HTTP API origin: {origin}");
    println!("  resolved from: profile");
    println!(
        "  api_key: {}",
        if resolve_api_key(profile).is_some() {
            "set"
        } else {
            "unset"
        }
    );
    println!(
        "  bearer_token: {}",
        if resolve_bearer(profile).is_some() {
            "set"
        } else {
            "unset"
        }
    );
    println!();
    let client = Client::builder()
        .build()
        .map_err(|e| anyhow!("http client: {e}"))?;
    match send_bytes(
        &client,
        &origin,
        profile,
        Method::GET,
        "/v1/health",
        None,
        None,
        None,
    )
    .await
    {
        Ok((st, _, _)) => println!("  GET /v1/health -> {st}"),
        Err(e) => println!("  GET /v1/health -> error: {e}"),
    }
    println!();
    println!("Agent flow: `search` → `context \"intent\" Cap …` → `run`");
    println!("Local state: ~/.plasm/cgs/servers/<slug>/current_session.txt → sessions/<client_session_id>/");
    Ok(())
}

/// Entry point for the `plasm` binary.
pub async fn run_terminal() -> Result<()> {
    crate::init_agent_runtime().map_err(|e| anyhow!("{e}"))?;
    let cli = Cli::parse();
    let mut profile = load_profile(cli.profile.as_str())?;

    match cli.cmd {
        Cmd::Init {
            server,
            api_key,
            bearer_token,
        } => run_init(
            cli.profile.as_str(),
            &mut profile,
            server,
            api_key,
            bearer_token,
        ),
        Cmd::Doctor => run_doctor(cli.profile.as_str(), &profile).await,
        Cmd::Search { intent, limit } => {
            let utterance = intent.trim().to_string();
            if utterance.is_empty() {
                return Err(anyhow!("search: intent text required"));
            }
            let server = require_configured_server(&profile)?;
            let client = Client::builder()
                .build()
                .map_err(|e| anyhow!("http client: {e}"))?;
            let payload = serde_json::to_vec(&TerminalDiscoverBody {
                intent: utterance.clone(),
                limit,
                allowed_entry_ids: vec![],
            })?;
            let (st, _, body) = send_bytes(
                &client,
                &server,
                &profile,
                Method::POST,
                "/v1/terminal/discover",
                Some("text/plain"),
                Some("application/json"),
                Some(payload),
            )
            .await?;
            if !st.is_success() {
                eprintln!("search: HTTP {}", st);
                std::io::stdout().write_all(&body)?;
                std::process::exit(1);
            }
            let md = String::from_utf8_lossy(&body);
            let disc = crate::terminal_state::discovery_from_search_markdown(&md, &utterance)?;
            let path = merge_and_write_latest_discovery(&server, &disc)?;
            eprintln!("discovery cache: {}", path.display());
            std::io::stdout().write_all(&body)?;
            if !body.ends_with(b"\n") {
                println!();
            }
            Ok(())
        }
        Cmd::Context {
            new,
            verbose,
            intent,
            capabilities,
        } => {
            let server = require_configured_server(&profile)?;
            let client = Client::builder()
                .build()
                .map_err(|e| anyhow!("http client: {e}"))?;
            run_context_command(
                &client,
                &server,
                &profile,
                new,
                verbose,
                intent,
                capabilities,
            )
            .await
        }
        Cmd::Run { mode, accept, file } => {
            let server = require_configured_server(&profile)?;
            let meta = resolve_current_session(&server)?;
            let mut sym = ClientSymbolSession::load_from_disk(&server, &meta.client_session_id)?;
            let body = read_program_body(file.as_ref())?;
            if body.is_empty() {
                return Err(anyhow!("run: empty program (stdin or --file)"));
            }
            let line =
                String::from_utf8(body).map_err(|_| anyhow!("run: program must be UTF-8"))?;
            let program = line.trim().to_string();
            let plan_json = sym
                .compile_program_to_plan(&program)
                .context("compile program to plan")?;
            parse_and_validate_plan_json(&plan_json).map_err(|e| anyhow!("plan: {e}"))?;
            let plan_bytes =
                serde_json::to_vec_pretty(&plan_json).map_err(|e| anyhow!("plan json: {e}"))?;
            let _ = write_session_file(
                server.as_str(),
                &sym.client_session_id,
                "latest_program.plasm",
                program.as_bytes(),
            );
            let _ = write_session_file(
                server.as_str(),
                &sym.client_session_id,
                "latest_plan.json",
                &plan_bytes,
            );
            let client = Client::builder()
                .build()
                .map_err(|e| anyhow!("http client: {e}"))?;
            let binding = ensure_execution_binding(&client, &server, &profile, &mut sym).await?;
            let ph = binding.prompt_hash.trim();
            let sid = binding.session.trim();
            let mode_trim = mode.trim().to_lowercase();
            let run_mode = if mode_trim == "plan" {
                ResolvedPlanRunMode::Plan
            } else {
                ResolvedPlanRunMode::Run
            };
            let req = ResolvedPlanRequest {
                protocol_version: ResolvedPlanProtocolVersion::V1.as_u16(),
                client_session_id: sym.client_session_id.clone(),
                catalog_pins: sym.catalog_pins(),
                mode: run_mode,
                source_program: program,
                plan: plan_json,
            };
            let path = format!("/execute/{ph}/{sid}/plan");
            let mut headers = HeaderMap::new();
            apply_auth_headers(&mut headers, &profile)?;
            headers.insert(
                ACCEPT,
                HeaderValue::from_str(accept.trim()).map_err(|e| anyhow!("accept: {e}"))?,
            );
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_str(RESOLVED_PLAN_CONTENT_TYPE)
                    .map_err(|e| anyhow!("content-type: {e}"))?,
            );
            let url = format!(
                "{}/{}",
                server.trim_end_matches('/'),
                path.trim_start_matches('/')
            );
            let res = client
                .post(url)
                .headers(headers)
                .body(serde_json::to_vec(&req).map_err(|e| anyhow!("request json: {e}"))?)
                .send()
                .await?;
            let st = res.status();
            let rh = res.headers().clone();
            let out = res.bytes().await?.to_vec();
            let label = if mode_trim == "plan" {
                "latest_plan.txt"
            } else {
                "latest_result.txt"
            };
            let mirror_resp =
                write_session_file(server.as_str(), &sym.client_session_id, label, &out)?;
            eprintln!("mirror: {}", mirror_resp.display());
            if !st.is_success() {
                eprintln!("run: HTTP {}", st);
                std::io::stdout().write_all(&out)?;
                std::process::exit(1);
            }
            std::io::stdout().write_all(&out)?;
            if !out.ends_with(b"\n") {
                println!();
            }
            if mode_trim != "plan" {
                if let Some(rid) = extract_run_id_from_response(&rh, &out) {
                    match mirror_run_snapshot(
                        &client,
                        &server,
                        &profile,
                        &sym.client_session_id,
                        ph,
                        sid,
                        &rid,
                    )
                    .await
                    {
                        Ok(p) => eprintln!("mirror run snapshot: {}", p.display()),
                        Err(e) => eprintln!("mirror run snapshot: {e}"),
                    }
                }
            }
            Ok(())
        }
    }
}
