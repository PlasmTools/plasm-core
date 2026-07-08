//! Local filesystem state for the agentic `plasm` CLI (client-owned symbol sessions).

use anyhow::{anyhow, bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::http_execute::CapabilitySeed;

pub use crate::catalog_pin::CatalogPin;

/// Client-owned symbol session metadata (no server `prompt_hash` as authority).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub client_session_id: String,
    pub intent: String,
    pub capabilities: Vec<(String, String)>,
    #[serde(default)]
    pub catalogs: Vec<CatalogPin>,
    /// Lazy server execute binding for HTTP run/plan (opaque execution handle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionBinding {
    pub prompt_hash: String,
    pub session: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryRow {
    pub row: usize,
    pub api: String,
    pub entity: String,
    pub description: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatestDiscovery {
    pub intent: Option<String>,
    pub rows: Vec<DiscoveryRow>,
}

/// Project-local CLI workspace root (default: current working directory).
///
/// Override with `PLASM_WORKSPACE` (tests, scripts).
pub fn workspace_dir() -> PathBuf {
    std::env::var_os("PLASM_WORKSPACE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Project-local Plasm CLI state root (`.plasm/` under [`workspace_dir`]).
pub fn plasm_root_dir() -> PathBuf {
    workspace_dir().join(".plasm")
}

pub fn profile_path(profile_name: &str) -> PathBuf {
    plasm_root_dir()
        .join("profiles")
        .join(format!("{profile_name}.json"))
}

pub fn language_frontmatter_markdown_path() -> PathBuf {
    plasm_root_dir().join("grammar.md")
}

pub fn plasm_cli_agent_skill_path() -> PathBuf {
    plasm_root_dir()
        .join("skills")
        .join("plasm-cli")
        .join("SKILL.md")
}

pub fn host_slug(server: &str) -> String {
    let h = Sha256::digest(server.as_bytes());
    hex::encode(h)[..8].to_string()
}

pub fn host_mirror_dir(server: &str) -> PathBuf {
    plasm_root_dir().join("hosts").join(host_slug(server))
}

pub fn server_mirror_dir(server: &str) -> PathBuf {
    host_mirror_dir(server)
}

pub fn discovery_cache_path(server: &str) -> PathBuf {
    host_mirror_dir(server).join("discovery.tsv")
}

pub fn latest_discovery_path(server: &str) -> PathBuf {
    discovery_cache_path(server)
}

pub fn current_session_pointer_path(server: &str) -> PathBuf {
    host_mirror_dir(server).join("current")
}

pub fn session_dir(client_session_id: &str) -> PathBuf {
    plasm_root_dir().join("s").join(client_session_id)
}

pub fn client_session_dir(_server: &str, client_session_id: &str) -> PathBuf {
    session_dir(client_session_id)
}

pub fn session_meta_path(_server: &str, client_session_id: &str) -> PathBuf {
    session_dir(client_session_id).join("meta.txt")
}

pub fn symbol_state_path(_server: &str, client_session_id: &str) -> PathBuf {
    session_dir(client_session_id).join("symbols.json")
}

pub fn teaching_tsv_path(_server: &str, client_session_id: &str) -> PathBuf {
    session_dir(client_session_id).join("teaching.tsv")
}

pub fn session_out_dir(client_session_id: &str) -> PathBuf {
    session_dir(client_session_id).join("out")
}

pub fn catalog_cache_path(_server: &str, client_session_id: &str, api: &str) -> PathBuf {
    session_dir(client_session_id)
        .join("catalogs")
        .join(format!("{api}.json"))
}

/// Short relative path for stderr `mirror:` lines (workspace-relative when possible).
pub fn display_mirror_path(path: &Path) -> String {
    let ws = workspace_dir();
    if let Ok(rel) = path.strip_prefix(&ws) {
        rel.display().to_string()
    } else if let Ok(rel) = path.strip_prefix(plasm_root_dir()) {
        format!(".plasm/{}", rel.display())
    } else {
        path.display().to_string()
    }
}

pub fn mint_client_session_id() -> String {
    hex::encode(Uuid::new_v4().as_bytes())[..8].to_string()
}

pub fn format_session_meta(meta: &SessionMeta) -> String {
    let mut out = format!(
        "client_session_id {}\nintent {}\n",
        meta.client_session_id, meta.intent
    );
    for pin in &meta.catalogs {
        out.push_str(&format!("catalog {} {}\n", pin.api, pin.digest));
    }
    for (api, entity) in &meta.capabilities {
        out.push_str(&format!("capability {api} {entity}\n"));
    }
    if let Some(ex) = &meta.execution {
        out.push_str(&format!("execution {} {}\n", ex.prompt_hash, ex.session));
    }
    out
}

pub fn parse_session_meta(raw: &str) -> Result<SessionMeta> {
    let mut client_session_id = None;
    let mut intent = None;
    let mut capabilities = Vec::new();
    let mut catalogs = Vec::new();
    let mut execution = None;

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or_default();
        match key {
            "client_session_id" => client_session_id = parts.next().map(str::to_string),
            "intent" => intent = Some(parts.collect::<Vec<_>>().join(" ")),
            "catalog" => {
                let api = parts.next().map(str::to_string);
                let digest = parts.next().map(str::to_string);
                if let (Some(a), Some(d)) = (api, digest) {
                    catalogs.push(CatalogPin { api: a, digest: d });
                }
            }
            "capability" => {
                let api = parts.next().map(str::to_string);
                let entity = parts.next().map(str::to_string);
                if let (Some(a), Some(e)) = (api, entity) {
                    capabilities.push((a, e));
                }
            }
            "execution" => {
                let ph = parts.next().map(str::to_string);
                let sid = parts.next().map(str::to_string);
                if let (Some(p), Some(s)) = (ph, sid) {
                    execution = Some(ExecutionBinding {
                        prompt_hash: p,
                        session: s,
                    });
                }
            }
            _ => {}
        }
    }

    Ok(SessionMeta {
        client_session_id: client_session_id
            .ok_or_else(|| anyhow!("session_meta: missing client_session_id"))?,
        intent: intent.unwrap_or_default(),
        capabilities,
        catalogs,
        execution,
    })
}

pub fn write_session_meta(server: &str, meta: &SessionMeta) -> Result<PathBuf> {
    let dir = client_session_dir(server, &meta.client_session_id);
    std::fs::create_dir_all(&dir)?;
    let path = session_meta_path(server, &meta.client_session_id);
    std::fs::write(&path, format_session_meta(meta))?;
    Ok(path)
}

pub fn read_session_meta(server: &str, client_session_id: &str) -> Result<SessionMeta> {
    let path = session_meta_path(server, client_session_id);
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    parse_session_meta(&raw)
}

pub fn write_current_session_pointer(server: &str, client_session_id: &str) -> Result<PathBuf> {
    let dir = host_mirror_dir(server);
    std::fs::create_dir_all(&dir)?;
    let path = current_session_pointer_path(server);
    std::fs::write(&path, format!("{client_session_id}\n"))?;
    Ok(path)
}

pub fn read_current_session_pointer(server: &str) -> Result<Option<String>> {
    let path = current_session_pointer_path(server);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(id) = line.strip_prefix("client_session_id ") {
            let id = id.trim();
            if !id.is_empty() {
                return Ok(Some(id.to_string()));
            }
        } else if !line.contains(char::is_whitespace) {
            return Ok(Some(line.to_string()));
        }
    }
    Ok(None)
}

pub fn resolve_current_session(server: &str) -> Result<SessionMeta> {
    let id = read_current_session_pointer(server)?.ok_or_else(|| {
        anyhow!(
            "No active plasm context for {server}. Run `plasm context -i \"…\" api:Entity …` first."
        )
    })?;
    read_session_meta(server, &id)
}

pub fn format_latest_discovery(disc: &LatestDiscovery) -> String {
    let mut out = String::new();
    if let Some(intent) = disc.intent.as_deref().filter(|s| !s.is_empty()) {
        out.push_str(&format!("intent\t{intent}\n"));
    }
    out.push_str("row\tapi\tentity\tdescription\n");
    for row in &disc.rows {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            row.row, row.api, row.entity, row.description
        ));
    }
    out
}

pub fn parse_latest_discovery(raw: &str) -> Result<LatestDiscovery> {
    let mut intent = None;
    let mut rows = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.first() == Some(&"intent") && cols.len() >= 2 {
            intent = Some(cols[1..].join("\t"));
            continue;
        }
        if cols.first() == Some(&"row") {
            continue;
        }
        if cols.len() >= 4 {
            let row_num: usize = cols[0].parse().unwrap_or(rows.len() + 1);
            rows.push(DiscoveryRow {
                row: row_num,
                api: cols[1].to_string(),
                entity: cols[2].to_string(),
                description: cols[3..].join("\t"),
            });
        } else if cols.len() == 3 {
            rows.push(DiscoveryRow {
                row: rows.len() + 1,
                api: cols[0].to_string(),
                entity: cols[1].to_string(),
                description: cols[2].to_string(),
            });
        }
    }
    Ok(LatestDiscovery { intent, rows })
}

/// Union discovery rows by `(api, entity)`; latest search intent wins.
pub fn merge_latest_discovery(
    existing: Option<&LatestDiscovery>,
    incoming: &LatestDiscovery,
) -> LatestDiscovery {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for row in incoming
        .rows
        .iter()
        .chain(existing.map(|d| d.rows.as_slice()).unwrap_or(&[]).iter())
    {
        let key = (row.api.as_str(), row.entity.as_str());
        if seen.insert(key) {
            rows.push(DiscoveryRow {
                row: rows.len() + 1,
                api: row.api.clone(),
                entity: row.entity.clone(),
                description: row.description.clone(),
            });
        }
    }
    LatestDiscovery {
        intent: incoming
            .intent
            .clone()
            .or_else(|| existing.and_then(|d| d.intent.clone())),
        rows,
    }
}

pub fn write_latest_discovery(server: &str, disc: &LatestDiscovery) -> Result<PathBuf> {
    let dir = server_mirror_dir(server);
    std::fs::create_dir_all(&dir)?;
    let path = latest_discovery_path(server);
    std::fs::write(&path, format_latest_discovery(disc))?;
    Ok(path)
}

pub fn read_latest_discovery(server: &str) -> Result<Option<LatestDiscovery>> {
    let path = latest_discovery_path(server);
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    Ok(Some(parse_latest_discovery(&raw)?))
}

pub fn merge_and_write_latest_discovery(
    server: &str,
    incoming: &LatestDiscovery,
) -> Result<PathBuf> {
    let existing = read_latest_discovery(server)?;
    let merged = merge_latest_discovery(existing.as_ref(), incoming);
    write_latest_discovery(server, &merged)
}

/// Extract the first fenced ` ```tsv ` block from discovery Markdown.
pub fn extract_discovery_tsv_block(markdown: &str) -> Option<String> {
    let needle = "```tsv";
    let start = markdown.find(needle)? + needle.len();
    let rest = &markdown[start..];
    let after_nl = rest.strip_prefix('\n').unwrap_or(rest);
    let end = after_nl.find("```")?;
    Some(after_nl[..end].trim_end().to_string())
}

pub fn discovery_from_search_markdown(markdown: &str, intent: &str) -> Result<LatestDiscovery> {
    let tsv = extract_discovery_tsv_block(markdown)
        .ok_or_else(|| anyhow!("search: no ```tsv block in discovery response"))?;
    let mut rows = Vec::new();
    let mut header = true;
    for line in tsv.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if header {
            header = false;
            if cols.first() == Some(&"api") {
                continue;
            }
        }
        if cols.len() >= 3 {
            rows.push(DiscoveryRow {
                row: rows.len() + 1,
                api: cols[0].to_string(),
                entity: cols[1].to_string(),
                description: cols[2..].join("\t"),
            });
        }
    }
    Ok(LatestDiscovery {
        intent: Some(intent.to_string()),
        rows,
    })
}

#[allow(dead_code)]
pub fn merge_capabilities(
    existing: &[(String, String)],
    added: &[(String, String)],
) -> Vec<(String, String)> {
    let mut out = existing.to_vec();
    for pair in added {
        if !out.iter().any(|p| p == pair) {
            out.push(pair.clone());
        }
    }
    out
}

fn push_qualified_seed(seeds: &mut Vec<CapabilitySeed>, name: &str) -> Result<()> {
    let (api, entity) = name
        .split_once(':')
        .ok_or_else(|| anyhow!("context: expected catalog:entity seed, got `{name}`"))?;
    let api = api.trim();
    let entity = entity.trim();
    if api.is_empty() || entity.is_empty() {
        bail!("context: invalid catalog:entity seed `{name}`");
    }
    seeds.push(CapabilitySeed {
        entry_id: api.to_string(),
        entity: entity.to_string(),
    });
    Ok(())
}

/// Resolve capability seeds from CLI names using `hosts/…/discovery.tsv` when needed.
pub fn resolve_capability_seeds(
    names: &[String],
    discovery: Option<&LatestDiscovery>,
    require_qualified: bool,
) -> Result<Vec<CapabilitySeed>> {
    if names.is_empty() {
        bail!("context: pass at least one catalog:entity seed (e.g. pokeapi:Pokemon)");
    }

    let all_qualified = names.iter().all(|n| {
        let n = n.trim();
        !n.is_empty() && n.contains(':')
    });

    if require_qualified {
        let mut seeds = Vec::new();
        for name in names {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            if !name.contains(':') {
                bail!(
                    "context --new requires catalog:entity seeds (e.g. pokeapi:Pokemon), not `{name}`"
                );
            }
            push_qualified_seed(&mut seeds, name)?;
        }
        if seeds.is_empty() {
            bail!("context: pass at least one catalog:entity seed");
        }
        return Ok(crate::http_execute::normalize_capability_seeds(seeds));
    }

    if all_qualified {
        let mut seeds = Vec::new();
        for name in names {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            push_qualified_seed(&mut seeds, name)?;
        }
        if seeds.is_empty() {
            bail!("context: pass at least one catalog:entity seed");
        }
        return Ok(crate::http_execute::normalize_capability_seeds(seeds));
    }

    let disc = discovery.ok_or_else(|| {
        anyhow!("context: no local discovery cache — run `plasm search \"…\"` first")
    })?;
    let mut seeds = Vec::new();
    for name in names {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        if let Some((api, entity)) = name.split_once(':') {
            let api = api.trim();
            let entity = entity.trim();
            if api.is_empty() || entity.is_empty() {
                bail!("context: invalid catalog:entity seed `{name}`");
            }
            seeds.push(CapabilitySeed {
                entry_id: api.to_string(),
                entity: entity.to_string(),
            });
            continue;
        }
        let matches: Vec<_> = disc
            .rows
            .iter()
            .filter(|r| r.entity.eq_ignore_ascii_case(name))
            .collect();
        match matches.len() {
            0 => bail!(
                "context: unknown capability `{name}` — run `plasm search` or qualify as api:Entity"
            ),
            1 => seeds.push(CapabilitySeed {
                entry_id: matches[0].api.clone(),
                entity: matches[0].entity.clone(),
            }),
            _ => {
                let options: Vec<String> = matches
                    .iter()
                    .map(|r| format!("{}:{}", r.api, r.entity))
                    .collect();
                bail!(
                    "context: ambiguous capability `{name}` — qualify one of: {}",
                    options.join(", ")
                );
            }
        }
    }
    if seeds.is_empty() {
        bail!("context: pass at least one catalog:entity seed");
    }
    Ok(crate::http_execute::normalize_capability_seeds(seeds))
}

#[allow(dead_code)]
pub fn seeds_to_capability_pairs(seeds: &[CapabilitySeed]) -> Vec<(String, String)> {
    seeds
        .iter()
        .map(|s| (s.entry_id.clone(), s.entity.clone()))
        .collect()
}

pub fn format_qualified_capabilities(capabilities: &[(String, String)]) -> String {
    capabilities
        .iter()
        .map(|(api, ent)| format!("{api}:{ent}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Append client-rendered teaching TSV rows to `teaching.tsv`.
pub fn append_teaching_tsv_wave(
    path: &Path,
    tsv_fragment: &str,
    first_write: bool,
) -> Result<usize> {
    let fragment = tsv_fragment.trim();
    if fragment.is_empty() {
        return Ok(0);
    }
    let mut lines_to_append = Vec::new();
    let mut seen_header = false;
    for line in fragment.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line == "plasm_expr\tMeaning" {
            seen_header = true;
            if first_write {
                lines_to_append.push(line.to_string());
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            if first_write && !seen_header {
                lines_to_append.push(line.to_string());
            }
            continue;
        }
        if line.contains('\t') {
            lines_to_append.push(line.to_string());
        }
    }
    let row_count = lines_to_append
        .iter()
        .filter(|l| l.contains('\t') && !l.starts_with('#') && *l != "plasm_expr\tMeaning")
        .count();
    if row_count == 0 {
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = if path.exists() && !first_write {
        std::fs::OpenOptions::new().append(true).open(path)?
    } else if first_write {
        std::fs::File::create(path)?
    } else {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
    };
    use std::io::Write;
    if !first_write && path.metadata().map(|m| m.len()).unwrap_or(0) > 0 {
        writeln!(file)?;
    }
    for line in &lines_to_append {
        writeln!(file, "{line}")?;
    }
    Ok(row_count)
}

pub fn format_language_frontmatter_markdown(frontmatter: &str) -> String {
    format!(
        "# Plasm Grammar\n\n\
This file is generated by `plasm init` so local agents can read the same \
canonical `plasm` MCP tool description (grammar contract) that production agents \
receive via MCP `tools/list`.\n\n\
```text\n{}\n```\n",
        frontmatter.trim()
    )
}

pub fn write_language_frontmatter_markdown(frontmatter: &str) -> Result<PathBuf> {
    let path = language_frontmatter_markdown_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format_language_frontmatter_markdown(frontmatter))?;
    Ok(path)
}

/// Cursor-style agent skill for the remote `plasm` HTTP terminal.
///
/// `frontmatter` must come from [`plasm_core::PLASM_TOOL_DESCRIPTION`]
/// so grammar stays aligned with MCP `tools/list` and `.plasm/grammar.md`.
pub fn format_plasm_cli_agent_skill(grammar_frontmatter: &str) -> String {
    format!(
        r#"---
name: plasm-cli
description: >-
  Operates the remote Plasm HTTP terminal (`plasm`): discovery, client-owned symbol context,
  plan-only dry runs, and live execution. Use when the user mentions `plasm init`, `plasm search`,
  `plasm context`, `plasm run`, `.plasm/` workspace state, teaching.tsv teaching tables, or remote
  Plasm execute sessions against plasm-mcp / plasm-server.
---

# Plasm CLI (remote HTTP terminal)

This file is **generated by `plasm init`**. Do not hand-edit the grammar block — it is copied from
the same renderer MCP hosts use at initialize time. For the standalone grammar file see
[`.plasm/grammar.md`](../grammar.md).

## When to use `plasm`

- **`plasm`** — remote HTTP terminal: discovery, client-owned `e#` / `m#` / `r#` symbols plus catalog wire names, plan/run
  against `plasm-mcp` or `plasm-server`.
- **`plasm-repl --schema …`** — local schema REPL (not the remote terminal).
- **`plasm-cgs`** (`plasm-cli` crate) — dev/schema tooling (not the remote terminal).

## Agent workflow

1. **`plasm init`** — configure profile under `.plasm/profiles/`; regenerates this skill and grammar.
2. **`plasm search "<intent>"`** — capability discovery; merges into `.plasm/hosts/<slug>/discovery.tsv`.
3. **`plasm context -i "<intent>" catalog:Entity …`** — expose entities; appends teaching rows to
   **`.plasm/s/<session>/teaching.tsv`** (client-owned symbol authority). With `--new`, every seed must
   be `entry_id:Entity` (e.g. `pokeapi:Pokemon`).
4. **`plasm run --mode plan`** — dry compile/validate only (no live side effects).
5. **`plasm run`** — live execution after reviewing the plan output.

Always **`plasm run --mode plan` before live `plasm run`** when side effects are possible.

## Read before writing programs

- **Active session pointer:** `.plasm/hosts/<slug>/current` (one line: client session id).
- **Teaching table (symbols):** `.plasm/s/<session>/teaching.tsv` — cumulative `plasm_expr` / `Meaning`
  rows; monotonic `e#` / `m#` / `r#` for that session; wire field/param names from the left column.
- **Session metadata:** `.plasm/s/<session>/meta.txt` — intent, catalog digests, capabilities.
- **Latest op mirror:** `.plasm/s/<session>/latest` → newest `out/NNNN-*` directory.

## Local mirror layout

```text
.plasm/
  profiles/<name>.json
  grammar.md
  skills/plasm-cli/SKILL.md          # this file
  hosts/<8hex>/
    discovery.tsv
    current
  s/<8hex>/
    meta.txt
    symbols.json
    teaching.tsv
    catalogs/<api>.json
    latest
    out/
      NNNN-search/    body.md, body.json
      NNNN-context/   wave.tsv, meta.json
      NNNN-plan/      program.plasm, plan.json, body.json, body.txt
      NNNN-run/       same + artifact.json, artifact.txt when available
```

After live **`plasm run`**, read **`body.txt`** / **`body.json`** in the latest `out/NNNN-run/` mirror.
When present, **`artifact.txt`** / **`artifact.json`** hold the full run snapshot mirrored locally.
Server-side durable artifact storage is configured separately (`PLASM_RUN_ARTIFACTS_URL`, etc.).

## Plasm syntax guide (generated)

```text
{grammar}
```
"#,
        grammar = grammar_frontmatter.trim()
    )
}

pub fn write_plasm_cli_agent_skill(grammar_frontmatter: &str) -> Result<PathBuf> {
    let path = plasm_cli_agent_skill_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format_plasm_cli_agent_skill(grammar_frontmatter))?;
    Ok(path)
}

#[cfg(test)]
pub(crate) mod test_env {
    use std::path::Path;
    use std::sync::{Mutex, MutexGuard};

    static PLASM_WORKSPACE_ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn plasm_workspace_env_lock() -> MutexGuard<'static, ()> {
        PLASM_WORKSPACE_ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Serialize tests that mutate `PLASM_WORKSPACE`; restores prior value on drop.
    pub(crate) fn with_plasm_workspace<R>(path: &Path, f: impl FnOnce() -> R) -> R {
        let _guard = plasm_workspace_env_lock();
        let prev = std::env::var_os("PLASM_WORKSPACE");
        std::env::set_var("PLASM_WORKSPACE", path);
        let out = f();
        match prev {
            Some(v) => std::env::set_var("PLASM_WORKSPACE", v),
            None => std::env::remove_var("PLASM_WORKSPACE"),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_dir_honors_plasm_workspace_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        let want = dir.path().join("proj");
        std::fs::create_dir_all(&want).expect("mkdir");
        super::test_env::with_plasm_workspace(&want, || {
            assert_eq!(workspace_dir(), want);
        });
    }

    #[test]
    fn display_mirror_path_under_workspace() {
        let dir = tempfile::tempdir().expect("tempdir");
        let want = dir.path().join("proj");
        std::fs::create_dir_all(&want).expect("mkdir");
        super::test_env::with_plasm_workspace(&want, || {
            let p = session_dir("abcd1234").join("out/0001-run/body.txt");
            let shown = display_mirror_path(&p);
            assert!(shown.contains(".plasm/s/abcd1234"));
        });
    }

    #[test]
    fn session_meta_roundtrip() {
        let meta = SessionMeta {
            client_session_id: "abcd1234".into(),
            intent: "inspect pokemon".into(),
            capabilities: vec![
                ("pokeapi".into(), "Pokemon".into()),
                ("pokeapi".into(), "Move".into()),
            ],
            catalogs: vec![CatalogPin {
                api: "pokeapi".into(),
                digest: "sha256:dead".into(),
            }],
            execution: None,
        };
        let raw = format_session_meta(&meta);
        let parsed = parse_session_meta(&raw).expect("parse");
        assert_eq!(parsed.client_session_id, "abcd1234");
        assert_eq!(parsed.capabilities.len(), 2);
        assert_eq!(parsed.catalogs[0].digest, "sha256:dead");
    }

    #[test]
    fn discovery_merge_unions_by_api_entity() {
        let a = LatestDiscovery {
            intent: Some("first".into()),
            rows: vec![DiscoveryRow {
                row: 1,
                api: "pokeapi".into(),
                entity: "Pokemon".into(),
                description: "a".into(),
            }],
        };
        let b = LatestDiscovery {
            intent: Some("second".into()),
            rows: vec![DiscoveryRow {
                row: 1,
                api: "pokeapi".into(),
                entity: "Move".into(),
                description: "b".into(),
            }],
        };
        let merged = merge_latest_discovery(Some(&a), &b);
        assert_eq!(merged.intent.as_deref(), Some("second"));
        assert_eq!(merged.rows.len(), 2);
        let entities: HashSet<_> = merged.rows.iter().map(|r| r.entity.as_str()).collect();
        assert!(entities.contains("Pokemon"));
        assert!(entities.contains("Move"));
    }

    #[test]
    fn resolve_unqualified_and_qualified_capabilities() {
        let disc = LatestDiscovery {
            intent: None,
            rows: vec![DiscoveryRow {
                row: 1,
                api: "pokeapi".into(),
                entity: "Pokemon".into(),
                description: String::new(),
            }],
        };
        let seeds = resolve_capability_seeds(&["Pokemon".into()], Some(&disc), false).expect("ok");
        assert_eq!(seeds[0].entry_id, "pokeapi");
    }

    #[test]
    fn resolve_qualified_without_discovery_cache() {
        let seeds = resolve_capability_seeds(
            &["pokeapi:Pokemon".into(), "pokeapi:Move".into()],
            None,
            false,
        )
        .expect("ok");
        assert_eq!(seeds.len(), 2);
        assert_eq!(seeds[0].entry_id, "pokeapi");
    }

    #[test]
    fn resolve_require_qualified_rejects_short_name() {
        let disc = LatestDiscovery {
            intent: None,
            rows: vec![DiscoveryRow {
                row: 1,
                api: "pokeapi".into(),
                entity: "Pokemon".into(),
                description: String::new(),
            }],
        };
        let err = resolve_capability_seeds(&["Pokemon".into()], Some(&disc), true).unwrap_err();
        assert!(err.to_string().contains("catalog:entity"));
    }

    #[test]
    fn append_teaching_tsv_skips_duplicate_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("teaching.tsv");
        let frag = "plasm_expr\tMeaning\ne1\treturns [e1]";
        let n = append_teaching_tsv_wave(&path, frag, true).unwrap();
        assert_eq!(n, 1);
        let n2 = append_teaching_tsv_wave(&path, "e2\treturns [e2]", false).unwrap();
        assert_eq!(n2, 1);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.matches("plasm_expr\tMeaning").count(), 1);
    }

    #[test]
    fn plasm_cli_agent_skill_path_under_plasm_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let want = dir.path().join("proj");
        std::fs::create_dir_all(&want).expect("mkdir");
        super::test_env::with_plasm_workspace(&want, || {
            let path = plasm_cli_agent_skill_path();
            assert_eq!(path, want.join(".plasm/skills/plasm-cli/SKILL.md"));
        });
    }

    #[test]
    fn format_plasm_cli_agent_skill_includes_frontmatter_and_workflow() {
        let grammar = "TSV table semantics:\nplasm_expr\tMeaning";
        let skill = format_plasm_cli_agent_skill(grammar);
        assert!(skill.starts_with("---\nname: plasm-cli\n"));
        assert!(skill.contains("description: >-"));
        assert!(skill.contains("plasm search"));
        assert!(skill.contains("plasm context"));
        assert!(skill.contains("plasm run --mode plan"));
        assert!(skill.contains(".plasm/s/"));
        assert!(skill.contains("artifact.json"));
        assert!(skill.contains(grammar));
    }

    #[test]
    fn write_plasm_cli_agent_skill_writes_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        super::test_env::with_plasm_workspace(dir.path(), || {
            let grammar = "contract line one";
            let path = write_plasm_cli_agent_skill(grammar).expect("write");
            assert!(path.exists());
            let raw = std::fs::read_to_string(&path).expect("read");
            assert!(raw.contains("name: plasm-cli"));
            assert!(raw.contains("contract line one"));
        });
    }
}
