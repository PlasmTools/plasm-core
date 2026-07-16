//! Remote HTTP terminal for `plasm`: discovery, client-owned context symbols, plan/run.
//!
//! See `docs/plasm-cgs-remote-terminal.md` in the parent repo.

use anyhow::{anyhow, Context as _, Result};
use clap::Parser;
use plasm_core::MutatorAdmit;
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
use crate::resolved_plan_http::{
    ResolvedPlanProtocolVersion, ResolvedPlanRequest, ResolvedPlanRunMode,
    RESOLVED_PLAN_CONTENT_TYPE,
};
use crate::terminal_cli::{validate_context_args, Cli, Cmd, RunModeCli};
use crate::terminal_mirror::{mirror_eprintln, MirrorOpKind, SessionMirror};
use crate::terminal_session::ClientSymbolSession;
use crate::terminal_state::{
    display_mirror_path, format_qualified_capabilities, merge_and_write_latest_discovery,
    mint_client_session_id, read_current_session_pointer, resolve_capability_seeds,
    resolve_current_session, write_current_session_pointer, write_language_frontmatter_markdown,
    write_plasm_cli_agent_skill, ExecutionBinding,
};
use plasm_core::PlasmComp;

/// Default HTTP origin written by `plasm init` when `--server` is omitted.
pub const DEFAULT_PLASM_HTTP_ORIGIN: &str = "http://127.0.0.1:3000";

/// Hosted Plasm **data-plane** HTTP base (`plasm init --server …` / device OAuth login).
///
/// SaaS ingress serves agent routes under `/plasm/http/…` on the public host (not bare `/v1/…`).
pub const DEFAULT_PLATFORM_HTTP_ORIGIN: &str = "https://platform.plasm.tools/plasm/http";

/// Legacy platform origin (profiles created before `/plasm/http` prefix).
pub const LEGACY_PLATFORM_HTTP_ORIGIN: &str = "https://platform.plasm.tools";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TerminalProfile {
    pub server: Option<String>,
    pub api_key: Option<String>,
    /// OAuth / GitHub sign-in JWT from [`run_device_login`] (stored as Bearer on HTTP).
    #[serde(alias = "bearer_token")]
    pub access_token: Option<String>,
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

fn profile_path(name: &str) -> PathBuf {
    crate::terminal_state::profile_path(name)
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

fn resolve_access_token(profile: &TerminalProfile) -> Option<String> {
    profile
        .access_token
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// True when the origin is the hosted platform (device OAuth), not a local appliance.
pub fn is_managed_platform_origin(server: &str) -> bool {
    let s = normalize_http_origin(server).to_ascii_lowercase();
    if s == DEFAULT_PLATFORM_HTTP_ORIGIN.to_ascii_lowercase()
        || s == LEGACY_PLATFORM_HTTP_ORIGIN.to_ascii_lowercase()
    {
        return true;
    }
    if let Ok(extra) = std::env::var("PLASM_CLI_PLATFORM_ORIGINS") {
        for o in extra.split(',') {
            let o = o.trim();
            if !o.is_empty() && normalize_http_origin(o).to_ascii_lowercase() == s {
                return true;
            }
        }
    }
    false
}

pub fn apply_auth_headers(headers: &mut HeaderMap, profile: &TerminalProfile) -> Result<()> {
    let api_key = resolve_api_key(profile);
    let bearer = resolve_access_token(profile);
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
    let path = profile_path(profile_name);
    save_profile(profile_name, profile)?;
    let grammar = plasm_core::PLASM_TOOL_DESCRIPTION;
    let grammar_path = write_language_frontmatter_markdown(grammar)?;
    let skill_path = write_plasm_cli_agent_skill(grammar)?;
    println!("configured {}", path.display());
    println!(
        "  workspace: {}",
        crate::terminal_state::plasm_root_dir().display()
    );
    println!("  grammar: {}", grammar_path.display());
    println!("  agent skill: {}", skill_path.display());
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
        "  access_token: {}",
        if profile.access_token.as_ref().is_some_and(|s| !s.is_empty()) {
            "set"
        } else {
            "unset"
        }
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct DeviceStartResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct DevicePollSuccess {
    access_token: String,
}

async fn run_device_login(profile_name: &str, profile: &mut TerminalProfile) -> Result<()> {
    if resolve_api_key(profile).is_some() {
        return Err(anyhow!(
            "device login is not used when an api_key is set in the profile"
        ));
    }
    let server = require_configured_server(profile)?;
    if !is_managed_platform_origin(&server) {
        return Err(anyhow!(
            "device login applies to managed platform hosts (e.g. {DEFAULT_PLATFORM_HTTP_ORIGIN}); \
             for local servers use `plasm init --server http://127.0.0.1:3000 --api-key …`"
        ));
    }

    let client = Client::builder()
        .build()
        .map_err(|e| anyhow!("http client: {e}"))?;
    let start_body = serde_json::json!({ "client_id": "plasm-cli" });
    let (st, _, body) = send_bytes(
        &client,
        &server,
        profile,
        Method::POST,
        "/v1/incoming-auth/device/start",
        Some("application/json"),
        Some("application/json"),
        Some(serde_json::to_vec(&start_body)?),
    )
    .await?;
    if !st.is_success() {
        let msg = String::from_utf8_lossy(&body);
        return Err(anyhow!("device login start failed HTTP {st}: {msg}"));
    }
    let start: DeviceStartResponse =
        serde_json::from_slice(&body).context("parse device start response")?;

    let open_url = start
        .verification_uri_complete
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(start.verification_uri.as_str());
    println!("Sign in to Plasm (device authorization)");
    println!("  user code: {}", start.user_code);
    println!("  open: {open_url}");
    println!("  (expires in {}s)", start.expires_in);
    let _ = std::process::Command::new("open").arg(open_url).status();

    let poll_body = serde_json::json!({ "device_code": start.device_code });
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(start.expires_in.max(60));
    let mut interval = start.interval.max(3);
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(anyhow!("device login timed out"));
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let (pst, _, pbody) = send_bytes(
            &client,
            &server,
            profile,
            Method::POST,
            "/v1/incoming-auth/device/poll",
            Some("application/json"),
            Some("application/json"),
            Some(serde_json::to_vec(&poll_body)?),
        )
        .await?;
        if pst.is_success() {
            if let Ok(ok) = serde_json::from_slice::<DevicePollSuccess>(&pbody) {
                profile.access_token = Some(ok.access_token);
                save_profile(profile_name, profile)?;
                println!(
                    "signed in — access_token saved to {}",
                    profile_path(profile_name).display()
                );
                return Ok(());
            }
        }
        if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&pbody) {
            if v.get("error").and_then(|e| e.as_str()) == Some("authorization_pending") {
                if let Some(i) = v.get("interval").and_then(|x| x.as_u64()) {
                    interval = i.max(1);
                }
                continue;
            }
            if v.get("error").and_then(|e| e.as_str()) == Some("expired_token") {
                return Err(anyhow!("device login expired; run `plasm login` again"));
            }
        }
        let msg = String::from_utf8_lossy(&pbody);
        return Err(anyhow!("device login poll failed HTTP {pst}: {msg}"));
    }
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

#[allow(clippy::too_many_arguments)]
async fn post_execute_program(
    client: &Client,
    server: &str,
    profile: &TerminalProfile,
    prompt_hash: &str,
    session: &str,
    program: &str,
    accept: &str,
    wait: bool,
    force: bool,
    plan_commit_ref: Option<&str>,
) -> Result<(StatusCode, HeaderMap, Vec<u8>)> {
    let mut query = format!("wait={wait}");
    if force {
        query.push_str("&force=true");
    }
    if let Some(pc) = plan_commit_ref.map(str::trim).filter(|s| !s.is_empty()) {
        query.push_str("&plan_commit_ref=");
        query.push_str(pc);
    }
    let path = format!("/execute/{prompt_hash}/{session}?{query}");
    send_bytes(
        client,
        server,
        profile,
        Method::POST,
        &path,
        Some(accept),
        Some("text/plain"),
        Some(program.as_bytes().to_vec()),
    )
    .await
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
        mutator_admit: MutatorAdmit::IntentOnly,
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
    eprintln!(
        "Active context: {}",
        format_qualified_capabilities(capabilities)
    );
    let shown = display_mirror_path(mirror);
    if rows_added > 0 {
        eprintln!("mirror: {shown} (+{rows_added} rows)");
    } else {
        eprintln!("mirror: {shown}");
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
    let seeds = resolve_capability_seeds(&capability_names, discovery.as_ref(), new_session)?;

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
        let intent = resolved_intent
            .clone()
            .ok_or_else(|| anyhow!("context: pass --intent (-i), or run `plasm search` first"))?;
        ClientSymbolSession::new(id, intent)
    } else if let Some(id) = read_current_session_pointer(server)? {
        ClientSymbolSession::load_from_disk(server, &id)?
    } else {
        let id = mint_client_session_id();
        let intent = resolved_intent
            .clone()
            .ok_or_else(|| anyhow!("context: pass --intent (-i), or run `plasm search` first"))?;
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
    let rows_added = if tsv_delta.is_empty() {
        0
    } else {
        sym.append_rendered_tsv(server, &tsv_delta)?.1
    };

    let mut session_mirror = SessionMirror::open(&sym.client_session_id)?;
    let op_dir = session_mirror.alloc_dir(MirrorOpKind::Context)?;
    let wave_path = session_mirror.write_file(&op_dir, "wave.tsv", tsv_delta.as_bytes())?;
    let meta_json = serde_json::json!({
        "intent": sym.intent,
        "capabilities": sym.capabilities.iter().map(|(a, e)| format!("{a}:{e}")).collect::<Vec<_>>(),
        "seeds": seeds.iter().map(|s| serde_json::json!({"api": s.entry_id, "entity": s.entity})).collect::<Vec<_>>(),
    });
    session_mirror.write_file(
        &op_dir,
        "meta.json",
        serde_json::to_string_pretty(&meta_json)?.as_bytes(),
    )?;
    let rel = session_mirror.rel_dir_for_display(&op_dir);
    session_mirror.update_latest_pointer(&rel)?;

    if !tsv_delta.is_empty() {
        if verbose {
            eprintln!("\n--- client exposure (symbol wave) ---");
        }
        print!("{tsv_delta}");
        if !tsv_delta.ends_with('\n') {
            println!();
        }
    }

    sym.persist(server)?;
    write_current_session_pointer(server, &sym.client_session_id)?;

    print_context_summary(&sym.capabilities, &wave_path, rows_added);
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

struct MirrorRunSnapshotCtx<'a> {
    server: &'a str,
    profile: &'a TerminalProfile,
    client_session_id: &'a str,
    prompt_hash: &'a str,
    session: &'a str,
    run_id: &'a str,
    op_dir: &'a Path,
}

async fn mirror_run_snapshot(client: &Client, ctx: &MirrorRunSnapshotCtx<'_>) -> Result<PathBuf> {
    let path = format!(
        "/execute/{}/{}/artifacts/{}",
        ctx.prompt_hash, ctx.session, ctx.run_id
    );
    let (st, _, body) = send_bytes(
        client,
        ctx.server,
        ctx.profile,
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
    let mirror = SessionMirror::open(ctx.client_session_id)?;
    mirror.write_artifact_pair(ctx.op_dir, &body)
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
            println!("Agent flow: `plasm init` → `search` → `plasm context -i \"…\" api:Entity …` → `run`");
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
        "  access_token: {}",
        if resolve_access_token(profile).is_some() {
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
    println!("Agent flow: `search` → `context -i \"…\" api:Entity …` → `run`");
    println!(
        "Local state: {}/hosts/<slug>/current → s/<session_id>/ (teaching.tsv, out/NNNN-*/)",
        crate::terminal_state::plasm_root_dir().display()
    );
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
            no_login,
        } => {
            run_init(cli.profile.as_str(), &mut profile, server, api_key)?;
            let origin = require_configured_server(&profile).ok();
            let should_login = !no_login
                && resolve_api_key(&profile).is_none()
                && resolve_access_token(&profile).is_none()
                && origin.as_deref().is_some_and(is_managed_platform_origin);
            if should_login {
                run_device_login(cli.profile.as_str(), &mut profile).await?;
            }
            Ok(())
        }
        Cmd::Login => run_device_login(cli.profile.as_str(), &mut profile).await,
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
            eprintln!("discovery cache: {}", display_mirror_path(&path));
            if let Some(sid) = read_current_session_pointer(&server)? {
                let mut session_mirror = SessionMirror::open(&sid)?;
                let op_dir = session_mirror.alloc_dir(MirrorOpKind::Search)?;
                session_mirror.write_file(&op_dir, "body.md", &body)?;
                let disc_json = serde_json::to_string_pretty(&disc)?;
                let json_path =
                    session_mirror.write_file(&op_dir, "body.json", disc_json.as_bytes())?;
                let rel = session_mirror.rel_dir_for_display(&op_dir);
                session_mirror.update_latest_pointer(&rel)?;
                mirror_eprintln(&json_path);
            }
            std::io::stdout().write_all(&body)?;
            if !body.ends_with(b"\n") {
                println!();
            }
            Ok(())
        }
        Cmd::Context { context } => {
            validate_context_args(&context)?;
            let server = require_configured_server(&profile)?;
            let client = Client::builder()
                .build()
                .map_err(|e| anyhow!("http client: {e}"))?;
            run_context_command(
                &client,
                &server,
                &profile,
                context.new,
                context.verbose,
                context.intent,
                context.seeds,
            )
            .await
        }
        Cmd::Run { run } => {
            let server = require_configured_server(&profile)?;
            let meta = resolve_current_session(&server)?;
            let mut sym = ClientSymbolSession::load_from_disk(&server, &meta.client_session_id)?;
            let body = read_program_body(run.file.as_ref())?;
            if body.is_empty() {
                return Err(anyhow!("run: empty program (stdin or --file)"));
            }
            let line =
                String::from_utf8(body).map_err(|_| anyhow!("run: program must be UTF-8"))?;
            let program = line.trim().to_string();
            let is_operation_continuation =
                program.starts_with("wait(") || program.starts_with("cancel(");
            let post_program_live =
                is_operation_continuation || (run.mode == RunModeCli::Run && !run.wait);
            let run_mode = run.mode.into();
            let mode_kind = if run_mode == ResolvedPlanRunMode::Plan {
                MirrorOpKind::Plan
            } else {
                MirrorOpKind::Run
            };
            let mut session_mirror = SessionMirror::open(&sym.client_session_id)?;
            let op_dir = session_mirror.alloc_dir(mode_kind)?;
            session_mirror.write_file(&op_dir, "program.plasm", program.as_bytes())?;
            let client = Client::builder()
                .build()
                .map_err(|e| anyhow!("http client: {e}"))?;
            let binding = ensure_execution_binding(&client, &server, &profile, &mut sym).await?;
            let ph = binding.prompt_hash.trim();
            let sid = binding.session.trim();
            if post_program_live {
                let (st, rh, out) = post_execute_program(
                    &client,
                    &server,
                    &profile,
                    ph,
                    sid,
                    &program,
                    run.accept.as_accept_header(),
                    run.wait,
                    run.force,
                    run.plan_commit_ref.as_deref(),
                )
                .await?;
                let accept_hint = run.accept.as_accept_header();
                let (_, body_txt) =
                    session_mirror.write_pair(&op_dir, "body", &out, Some(accept_hint))?;
                let rel = session_mirror.rel_dir_for_display(&op_dir);
                session_mirror.update_latest_pointer(&rel)?;
                mirror_eprintln(&body_txt);
                if !st.is_success() {
                    eprintln!("run: HTTP {}", st);
                    std::io::stdout().write_all(&out)?;
                    std::process::exit(1);
                }
                std::io::stdout().write_all(&out)?;
                if !out.ends_with(b"\n") {
                    println!();
                }
                if run_mode != ResolvedPlanRunMode::Plan {
                    if let Some(rid) = extract_run_id_from_response(&rh, &out) {
                        let _ = mirror_run_snapshot(
                            &client,
                            &MirrorRunSnapshotCtx {
                                server: &server,
                                profile: &profile,
                                client_session_id: &sym.client_session_id,
                                prompt_hash: ph,
                                session: sid,
                                run_id: &rid,
                                op_dir: &op_dir,
                            },
                        )
                        .await;
                    }
                }
                return Ok(());
            }
            let comp_json = sym
                .compile_program_to_plan(&program)
                .context("compile program to comp")?;
            let comp: PlasmComp =
                serde_json::from_value(comp_json.clone()).context("decode comp")?;
            comp.validate().map_err(|e| anyhow!("comp: {e}"))?;
            let comp_bytes =
                serde_json::to_vec_pretty(&comp_json).map_err(|e| anyhow!("comp json: {e}"))?;
            session_mirror.write_file(&op_dir, "comp.json", &comp_bytes)?;
            let req = ResolvedPlanRequest {
                protocol_version: ResolvedPlanProtocolVersion::V1.as_u16(),
                client_session_id: sym.client_session_id.clone(),
                catalog_pins: sym.catalog_pins(),
                mode: run_mode,
                source_program: program,
                comp,
            };
            let path = format!("/execute/{ph}/{sid}/plan");
            let mut headers = HeaderMap::new();
            apply_auth_headers(&mut headers, &profile)?;
            headers.insert(
                ACCEPT,
                HeaderValue::from_static(run.accept.as_accept_header()),
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
            let accept_hint = run.accept.as_accept_header();
            let (_, body_txt) =
                session_mirror.write_pair(&op_dir, "body", &out, Some(accept_hint))?;
            let rel = session_mirror.rel_dir_for_display(&op_dir);
            session_mirror.update_latest_pointer(&rel)?;
            mirror_eprintln(&body_txt);
            if !st.is_success() {
                eprintln!("run: HTTP {}", st);
                std::io::stdout().write_all(&out)?;
                std::process::exit(1);
            }
            std::io::stdout().write_all(&out)?;
            if !out.ends_with(b"\n") {
                println!();
            }
            if run_mode != ResolvedPlanRunMode::Plan {
                if let Some(rid) = extract_run_id_from_response(&rh, &out) {
                    match mirror_run_snapshot(
                        &client,
                        &MirrorRunSnapshotCtx {
                            server: &server,
                            profile: &profile,
                            client_session_id: &sym.client_session_id,
                            prompt_hash: ph,
                            session: sid,
                            run_id: &rid,
                            op_dir: &op_dir,
                        },
                    )
                    .await
                    {
                        Ok(p) => mirror_eprintln(&p),
                        Err(e) => eprintln!("mirror run snapshot: {e}"),
                    }
                }
            }
            Ok(())
        }
        Cmd::Evidence { cmd } => run_evidence_cmd(cmd),
    }
}

fn run_evidence_cmd(cmd: crate::terminal_cli::EvidenceCmd) -> Result<(), anyhow::Error> {
    use crate::evidence_chain::trusted_public_keys_from_env;
    use crate::terminal_cli::EvidenceCmd;
    use plasm_evidence::{
        run_seal_inputs_from_artifact, DefaultChainVerifier, RunArtifactForSeal, VerifyOptions,
    };
    use std::io::Read;

    match cmd {
        EvidenceCmd::Verify {
            path,
            run_id,
            artifact,
            schema: _schema,
            trusted_pubkey,
        } => {
            let mut raw = String::new();
            std::fs::File::open(&path)
                .map_err(|e| anyhow!("evidence verify: open {}: {e}", path.display()))?
                .read_to_string(&mut raw)
                .map_err(|e| anyhow!("evidence verify: read {}: {e}", path.display()))?;
            let bundle: plasm_evidence::EvidenceBundle = serde_json::from_str(&raw)
                .map_err(|e| anyhow!("evidence verify: decode {}: {e}", path.display()))?;
            let mut trusted = trusted_public_keys_from_env();
            trusted.extend(
                trusted_pubkey
                    .into_iter()
                    .map(|k| k.trim().to_ascii_lowercase())
                    .filter(|k| !k.is_empty()),
            );
            trusted.sort();
            trusted.dedup();
            let opts = VerifyOptions {
                trusted_public_keys: trusted,
            };
            DefaultChainVerifier::verify_bundle_for_serve(&bundle, &opts)
                .map_err(|e| anyhow!("evidence verify: {e}"))?;
            if let Some(rid) = run_id.as_deref().filter(|s| !s.trim().is_empty()) {
                let artifact_path = artifact.as_ref().ok_or_else(|| {
                    anyhow!("evidence verify: --run-id requires --artifact for digest verification")
                })?;
                let mut artifact_raw = String::new();
                std::fs::File::open(artifact_path)
                    .map_err(|e| {
                        anyhow!(
                            "evidence verify: open artifact {}: {e}",
                            artifact_path.display()
                        )
                    })?
                    .read_to_string(&mut artifact_raw)
                    .map_err(|e| {
                        anyhow!(
                            "evidence verify: read artifact {}: {e}",
                            artifact_path.display()
                        )
                    })?;
                let artifact_doc: RunArtifactForSeal = serde_json::from_str(&artifact_raw)
                    .map_err(|e| {
                        anyhow!(
                            "evidence verify: decode artifact {}: {e}",
                            artifact_path.display()
                        )
                    })?;
                let source_line = artifact_doc.source_line();
                let inputs = run_seal_inputs_from_artifact(
                    &bundle.scope,
                    &artifact_doc,
                    &source_line,
                    &artifact_doc.parsed_preimage,
                );
                DefaultChainVerifier::verify_run_seal_with_inputs(&bundle, rid, &inputs)
                    .map_err(|e| anyhow!("evidence verify: run_sealed digest failed: {e}"))?;
                println!("ok: chain + topo + run_sealed verified for {rid}");
            } else {
                println!("ok: chain + step topo verified");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod platform_origin_tests {
    use super::{
        is_managed_platform_origin, DEFAULT_PLATFORM_HTTP_ORIGIN, LEGACY_PLATFORM_HTTP_ORIGIN,
    };

    #[test]
    fn managed_platform_accepts_current_and_legacy_origins() {
        assert!(is_managed_platform_origin(DEFAULT_PLATFORM_HTTP_ORIGIN));
        assert!(is_managed_platform_origin(LEGACY_PLATFORM_HTTP_ORIGIN));
        assert!(is_managed_platform_origin(
            "https://platform.plasm.tools/plasm/http/"
        ));
        assert!(!is_managed_platform_origin("http://127.0.0.1:3000"));
    }
}
