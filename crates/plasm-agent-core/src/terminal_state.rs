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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryRow {
    pub row: usize,
    pub api: String,
    pub entity: String,
    pub description: String,
}

#[derive(Debug, Clone, Default)]
pub struct LatestDiscovery {
    pub intent: Option<String>,
    pub rows: Vec<DiscoveryRow>,
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn cgs_root_dir() -> PathBuf {
    home_dir().join(".plasm/cgs")
}

pub fn language_frontmatter_markdown_path() -> PathBuf {
    cgs_root_dir().join("plasm_grammar.md")
}

pub fn server_slug(server: &str) -> String {
    let h = Sha256::digest(server.as_bytes());
    hex::encode(h)[..12].to_string()
}

pub fn server_mirror_dir(server: &str) -> PathBuf {
    home_dir()
        .join(".plasm/cgs/servers")
        .join(server_slug(server))
}

pub fn latest_discovery_path(server: &str) -> PathBuf {
    server_mirror_dir(server).join("latest_discovery.tsv")
}

pub fn current_session_pointer_path(server: &str) -> PathBuf {
    server_mirror_dir(server).join("current_session.txt")
}

pub fn client_session_dir(server: &str, client_session_id: &str) -> PathBuf {
    server_mirror_dir(server)
        .join("sessions")
        .join(client_session_id)
}

pub fn session_meta_path(server: &str, client_session_id: &str) -> PathBuf {
    client_session_dir(server, client_session_id).join("session_meta.txt")
}

pub fn symbol_state_path(server: &str, client_session_id: &str) -> PathBuf {
    client_session_dir(server, client_session_id).join("symbol_state.json")
}

pub fn domain_tsv_path(server: &str, client_session_id: &str) -> PathBuf {
    client_session_dir(server, client_session_id).join("domain.tsv")
}

pub fn catalog_cache_path(server: &str, client_session_id: &str, api: &str) -> PathBuf {
    client_session_dir(server, client_session_id)
        .join("catalogs")
        .join(format!("{api}.json"))
}

pub fn mint_client_session_id() -> String {
    format!("cs_{}", Uuid::new_v4().simple())
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
    let dir = server_mirror_dir(server);
    std::fs::create_dir_all(&dir)?;
    let path = current_session_pointer_path(server);
    std::fs::write(&path, format!("client_session_id {client_session_id}\n"))?;
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
        if let Some(id) = line.strip_prefix("client_session_id ") {
            let id = id.trim();
            if !id.is_empty() {
                return Ok(Some(id.to_string()));
            }
        }
    }
    Ok(None)
}

pub fn resolve_current_session(server: &str) -> Result<SessionMeta> {
    let id = read_current_session_pointer(server)?.ok_or_else(|| {
        anyhow!(
            "No active plasm context for {server}. Run `plasm context \"intent\" CapabilityName ...` first."
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

/// Resolve positional capability names using `latest_discovery.tsv`.
pub fn resolve_capability_seeds(
    names: &[String],
    discovery: Option<&LatestDiscovery>,
) -> Result<Vec<CapabilitySeed>> {
    if names.is_empty() {
        bail!("context: pass at least one capability name (e.g. Pokemon Move Type)");
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
            seeds.push(CapabilitySeed {
                entry_id: api.trim().to_string(),
                entity: entity.trim().to_string(),
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
        bail!("context: pass at least one capability name");
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

/// Append client-rendered teaching TSV rows to `domain.tsv`.
pub fn append_domain_tsv_wave(path: &Path, tsv_fragment: &str, first_write: bool) -> Result<usize> {
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

pub fn write_session_file(
    server: &str,
    client_session_id: &str,
    label: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    let dir = client_session_dir(server, client_session_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(label);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

pub fn format_language_frontmatter_markdown(frontmatter: &str) -> String {
    format!(
        "# Plasm Grammar\n\n\
This file is generated by `plasm init` so local agents can read the same \
frontmatter teaching grammar that Plasm exposes through MCP initialize \
instructions.\n\n\
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_meta_roundtrip() {
        let meta = SessionMeta {
            client_session_id: "cs_abc".into(),
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
        assert_eq!(parsed.client_session_id, "cs_abc");
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
        let seeds = resolve_capability_seeds(&["Pokemon".into()], Some(&disc)).expect("ok");
        assert_eq!(seeds[0].entry_id, "pokeapi");
    }

    #[test]
    fn append_domain_tsv_skips_duplicate_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("domain.tsv");
        let frag = "plasm_expr\tMeaning\ne1\treturns [e1]";
        let n = append_domain_tsv_wave(&path, frag, true).unwrap();
        assert_eq!(n, 1);
        let n2 = append_domain_tsv_wave(&path, "e2\treturns [e2]", false).unwrap();
        assert_eq!(n2, 1);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.matches("plasm_expr\tMeaning").count(), 1);
    }
}
