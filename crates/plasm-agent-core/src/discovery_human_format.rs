//! Single discovery surface for agents: MCP `discover_capabilities` (non-typed) and HTTP terminal discover
//! both render via [`format_discovery_markdown`].

use std::collections::{BTreeSet, HashMap};

use indexmap::IndexMap;
use plasm_core::discovery::{Ambiguity, DiscoveryResult, EntitySummary, RankedCandidate};

/// MCP entity `description` column: max chars (Unicode scalars).
pub const MCP_DISCOVERY_ENTITY_SUMMARY_MAX: usize = 200;

/// Default max rows in MCP `discover_capabilities` TSV (score-ranked).
pub const MCP_DISCOVERY_DEFAULT_MAX_ROWS: usize = 12;

/// Default max rows per registry `entry_id` in MCP discovery TSV.
pub const MCP_DISCOVERY_DEFAULT_MAX_PER_ENTRY: usize = 8;

/// How capped MCP discovery rows are chosen from score-ranked deduped candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiscoveryTableMode {
    /// First `max_rows` rows in global score order (default for MCP).
    #[default]
    GlobalTopN,
    /// Round-robin across catalogs up to `max_per_entry` per `entry_id` (legacy fair-share).
    #[allow(dead_code)] // selected via `DiscoveryTablePolicy` (tests + explicit policy)
    PerEntryFairShare,
}

/// Row cap and per-catalog limits for MCP discovery tables (HTTP terminal uses uncapped [`format_discovery_markdown`]).
#[derive(Debug, Clone, Copy)]
pub struct DiscoveryTablePolicy {
    pub mode: DiscoveryTableMode,
    pub max_rows: usize,
    pub max_per_entry: Option<usize>,
}

impl Default for DiscoveryTablePolicy {
    fn default() -> Self {
        Self {
            mode: DiscoveryTableMode::GlobalTopN,
            max_rows: MCP_DISCOVERY_DEFAULT_MAX_ROWS,
            max_per_entry: Some(MCP_DISCOVERY_DEFAULT_MAX_PER_ENTRY),
        }
    }
}

/// Omission stats for `_meta.plasm.discovery` on capped MCP responses.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveryOmissionMeta {
    pub truncated: bool,
    pub shown: usize,
    pub omitted: usize,
    pub top_omitted: Vec<(String, String)>,
}

/// MCP discovery markdown plus omission metadata.
#[derive(Debug, Clone)]
pub struct FormattedDiscovery {
    pub markdown: String,
    pub omission: DiscoveryOmissionMeta,
}

/// Single-line TSV field: collapse whitespace, strip tabs, truncate (Unicode scalars).
fn mcp_discovery_tsv_field(s: &str, max_chars: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let no_tabs = collapsed.replace('\t', " ");
    let n = no_tabs.chars().count();
    if n <= max_chars {
        no_tabs
    } else {
        let head: String = no_tabs.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn entity_summary_description<'a>(
    entity_summaries: &'a [EntitySummary],
    entry_id: &str,
    entity: &str,
) -> Option<&'a str> {
    entity_summaries
        .iter()
        .find(|e| e.entry_id == entry_id && e.name == entity)
        .map(|e| e.description.as_str())
}

/// Dedupe `(entry_id, entity)` preserving first (highest score) candidate order.
fn ranked_deduped_entity_rows(candidates: &[RankedCandidate]) -> Vec<(String, String)> {
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for c in candidates {
        let key = (c.entry_id.as_str(), c.entity.as_str());
        if seen.insert(key) {
            rows.push((c.entry_id.clone(), c.entity.clone()));
        }
    }
    rows
}

fn apply_discovery_table_policy(
    rows: Vec<(String, String)>,
    policy: &DiscoveryTablePolicy,
) -> (Vec<(String, String)>, DiscoveryOmissionMeta) {
    if rows.is_empty() {
        return (Vec::new(), DiscoveryOmissionMeta::default());
    }

    match policy.mode {
        DiscoveryTableMode::GlobalTopN => apply_global_top_n_policy(rows, policy),
        DiscoveryTableMode::PerEntryFairShare => apply_per_entry_fair_share_policy(rows, policy),
    }
}

fn apply_global_top_n_policy(
    rows: Vec<(String, String)>,
    policy: &DiscoveryTablePolicy,
) -> (Vec<(String, String)>, DiscoveryOmissionMeta) {
    let mut per_entry: HashMap<String, usize> = HashMap::new();
    let mut shown = Vec::new();
    let mut omitted = Vec::new();
    for row in rows {
        if shown.len() >= policy.max_rows {
            omitted.push(row);
            continue;
        }
        if let Some(cap) = policy.max_per_entry {
            let n = per_entry.entry(row.0.clone()).or_insert(0);
            if *n >= cap {
                omitted.push(row);
                continue;
            }
            *n += 1;
        }
        shown.push(row);
    }
    let omission = DiscoveryOmissionMeta {
        truncated: !omitted.is_empty(),
        shown: shown.len(),
        omitted: omitted.len(),
        top_omitted: omitted.into_iter().take(5).collect(),
    };
    (shown, omission)
}

fn apply_per_entry_fair_share_policy(
    rows: Vec<(String, String)>,
    policy: &DiscoveryTablePolicy,
) -> (Vec<(String, String)>, DiscoveryOmissionMeta) {
    let mut groups: IndexMap<String, Vec<(String, String)>> = IndexMap::new();
    for row in rows {
        groups.entry(row.0.clone()).or_default().push(row);
    }
    let catalog_ids: Vec<String> = groups.keys().cloned().collect();

    let mut per_entry: HashMap<String, usize> = HashMap::new();
    let mut shown = Vec::new();
    let mut omitted = Vec::new();

    loop {
        if shown.len() >= policy.max_rows {
            break;
        }
        let mut picked_any = false;
        for eid in &catalog_ids {
            if shown.len() >= policy.max_rows {
                break;
            }
            if let Some(cap) = policy.max_per_entry {
                if per_entry.get(eid).copied().unwrap_or(0) >= cap {
                    continue;
                }
            }
            let Some(queue) = groups.get_mut(eid) else {
                continue;
            };
            if queue.is_empty() {
                continue;
            }
            let row = queue.remove(0);
            *per_entry.entry(eid.clone()).or_insert(0) += 1;
            shown.push(row);
            picked_any = true;
        }
        if !picked_any {
            break;
        }
    }

    for queue in groups.values_mut() {
        for row in queue.drain(..) {
            omitted.push(row);
        }
    }

    let omission = DiscoveryOmissionMeta {
        truncated: !omitted.is_empty(),
        shown: shown.len(),
        omitted: omitted.len(),
        top_omitted: omitted.into_iter().take(5).collect(),
    };
    (shown, omission)
}

/// TSV: `api`, `entity`, `description` — dedupe `(entry_id, entity)` from ranked `candidates`.
#[allow(dead_code)]
pub fn discovery_capability_tsv_for_candidates(
    candidates: &[RankedCandidate],
    entity_summaries: &[EntitySummary],
) -> String {
    discovery_capability_tsv_for_rows(&ranked_deduped_entity_rows(candidates), entity_summaries)
}

fn discovery_capability_tsv_for_rows(
    rows: &[(String, String)],
    entity_summaries: &[EntitySummary],
) -> String {
    let mut lines = vec!["api\tentity\tdescription".to_string()];
    for (eid, entity) in rows {
        let description = entity_summary_description(entity_summaries, eid, entity)
            .map(|raw| mcp_discovery_tsv_field(raw, MCP_DISCOVERY_ENTITY_SUMMARY_MAX))
            .unwrap_or_default();
        lines.push(format!(
            "{}\t{}\t{}",
            mcp_discovery_tsv_field(eid, 200),
            mcp_discovery_tsv_field(entity, 200),
            description,
        ));
    }
    lines.join("\n")
}

/// Full structured discovery result (all ranked candidates).
#[allow(dead_code)]
pub fn discovery_capability_tsv(result: &DiscoveryResult) -> String {
    discovery_capability_tsv_for_candidates(&result.candidates, &result.entity_summaries)
}

fn ambiguity_markdown_lines(ambiguities: &[Ambiguity]) -> String {
    let mut s = String::new();
    for Ambiguity {
        dimension: _,
        entry_ids,
        capability_name,
        score,
    } in ambiguities
    {
        s.push_str(&format!(
            "- `{capability_name}` (score {score}) — pick one `api`: {}\n",
            entry_ids.join(", ")
        ));
    }
    s
}

/// Markdown block for ambiguities (same text as MCP `discover_capabilities`).
pub fn discovery_ambiguity_markdown(result: &DiscoveryResult) -> String {
    if result.ambiguities.is_empty() {
        return String::new();
    }
    let mut s = String::from("**Same name in more than one `api`**\n\n");
    s.push_str(&ambiguity_markdown_lines(&result.ambiguities));
    s.push('\n');
    s
}

fn discovery_markdown_body(
    tsv: &str,
    result: &DiscoveryResult,
    omission: &DiscoveryOmissionMeta,
) -> String {
    let mut s = String::new();
    if tsv.lines().count() <= 1 {
        s.push_str("_No matching entities._\n\n");
    } else {
        s.push_str("```tsv\n");
        s.push_str(tsv);
        s.push_str("\n```\n\n");
        if omission.truncated {
            s.push_str(&format!(
                "_Showing top {} discovery rows ({} omitted). Narrow `intent` or pass seeds you already know._\n\n",
                omission.shown, omission.omitted
            ));
        }
    }
    s.push_str(&discovery_ambiguity_markdown(result));
    s
}

/// MCP `discover_capabilities` (non-typed) and `POST /v1/terminal/discover`: fenced TSV + ambiguity notes.
pub fn format_discovery_markdown(result: &DiscoveryResult) -> String {
    format_discovery_markdown_for_mcp(
        result,
        &DiscoveryTablePolicy {
            mode: DiscoveryTableMode::GlobalTopN,
            max_rows: usize::MAX,
            max_per_entry: None,
        },
    )
    .markdown
}

/// MCP `discover_capabilities` with score-ranked row caps and omission metadata.
pub fn format_discovery_markdown_for_mcp(
    result: &DiscoveryResult,
    policy: &DiscoveryTablePolicy,
) -> FormattedDiscovery {
    let ranked = ranked_deduped_entity_rows(&result.candidates);
    let (shown, omission) = apply_discovery_table_policy(ranked, policy);
    let tsv = discovery_capability_tsv_for_rows(&shown, &result.entity_summaries);
    let markdown = discovery_markdown_body(&tsv, result, &omission);
    FormattedDiscovery { markdown, omission }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::discovery::CapabilityQuery;

    fn sample_result(candidates: Vec<RankedCandidate>) -> DiscoveryResult {
        DiscoveryResult {
            contexts: vec![],
            candidates,
            ambiguities: vec![],
            applied_query_echo: CapabilityQuery::default(),
            closure_stats: None,
            schema_neighborhoods: vec![],
            entity_summaries: vec![
                EntitySummary {
                    entry_id: "demo".into(),
                    name: "Widget".into(),
                    description: "Widget summary.".into(),
                },
                EntitySummary {
                    entry_id: "demo".into(),
                    name: "Gadget".into(),
                    description: "Gadget summary.".into(),
                },
                EntitySummary {
                    entry_id: "demo".into(),
                    name: "Gizmo".into(),
                    description: "Gizmo summary.".into(),
                },
            ],
        }
    }

    #[test]
    fn discovery_capability_tsv_header_and_row() {
        let r = sample_result(vec![RankedCandidate {
            entry_id: "demo".into(),
            entity: "Widget".into(),
            capability_name: "list".into(),
            score: 2,
            reason_codes: vec![],
            capability_description: "List widgets".into(),
        }]);
        let tsv = discovery_capability_tsv(&r);
        assert!(tsv.starts_with("api\tentity\tdescription\n"));
        assert!(tsv.contains("demo\tWidget\tWidget summary."));
    }

    #[test]
    fn format_discovery_markdown_matches_mcp_non_typed_shape() {
        let r = sample_result(vec![RankedCandidate {
            entry_id: "demo".into(),
            entity: "Widget".into(),
            capability_name: "list".into(),
            score: 2,
            reason_codes: vec![],
            capability_description: "List widgets".into(),
        }]);
        let md = format_discovery_markdown(&r);
        assert!(md.contains("```tsv"));
        assert!(md.contains("demo\tWidget\tWidget summary."));
        assert!(!md.contains("typed:"));
    }

    #[test]
    fn mcp_discovery_preserves_score_order() {
        let r = sample_result(vec![
            RankedCandidate {
                entry_id: "b".into(),
                entity: "Gadget".into(),
                capability_name: "list".into(),
                score: 10,
                reason_codes: vec![],
                capability_description: String::new(),
            },
            RankedCandidate {
                entry_id: "a".into(),
                entity: "Widget".into(),
                capability_name: "list".into(),
                score: 5,
                reason_codes: vec![],
                capability_description: String::new(),
            },
        ]);
        let formatted = format_discovery_markdown_for_mcp(&r, &DiscoveryTablePolicy::default());
        let lines: Vec<_> = formatted
            .markdown
            .lines()
            .filter(|l| l.contains('\t') && !l.starts_with("api\t"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("b\tGadget"));
        assert!(lines[1].starts_with("a\tWidget"));
    }

    #[test]
    fn mcp_discovery_row_cap_omits_tail() {
        let r = sample_result(
            (0..20)
                .map(|i| RankedCandidate {
                    entry_id: format!("api{i}"),
                    entity: format!("Entity{i}"),
                    capability_name: "list".into(),
                    score: 100 - i,
                    reason_codes: vec![],
                    capability_description: String::new(),
                })
                .collect(),
        );
        let formatted = format_discovery_markdown_for_mcp(
            &r,
            &DiscoveryTablePolicy {
                mode: DiscoveryTableMode::GlobalTopN,
                max_rows: 3,
                max_per_entry: None,
            },
        );
        assert!(formatted.omission.truncated);
        assert_eq!(formatted.omission.shown, 3);
        assert_eq!(formatted.omission.omitted, 17);
        assert!(formatted.markdown.contains("17 omitted"));
    }

    #[test]
    fn mcp_discovery_global_top_n_prefers_high_scores() {
        let r = sample_result(vec![
            RankedCandidate {
                entry_id: "github".into(),
                entity: "Repository".into(),
                capability_name: "repo_search".into(),
                score: 100,
                reason_codes: vec![],
                capability_description: String::new(),
            },
            RankedCandidate {
                entry_id: "proof".into(),
                entity: "Project".into(),
                capability_name: "list".into(),
                score: 5,
                reason_codes: vec![],
                capability_description: String::new(),
            },
            RankedCandidate {
                entry_id: "cloudflare".into(),
                entity: "Zone".into(),
                capability_name: "list".into(),
                score: 4,
                reason_codes: vec![],
                capability_description: String::new(),
            },
        ]);
        let formatted = format_discovery_markdown_for_mcp(
            &r,
            &DiscoveryTablePolicy {
                mode: DiscoveryTableMode::GlobalTopN,
                max_rows: 2,
                max_per_entry: None,
            },
        );
        let lines: Vec<_> = formatted
            .markdown
            .lines()
            .filter(|l| l.contains('\t') && !l.starts_with("api\t"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("github\tRepository"));
        assert!(lines[1].starts_with("proof\tProject"));
    }

    #[test]
    fn mcp_discovery_federated_fair_share_includes_each_catalog() {
        let r = sample_result(vec![
            RankedCandidate {
                entry_id: "github".into(),
                entity: "Repository".into(),
                capability_name: "repo_search".into(),
                score: 100,
                reason_codes: vec![],
                capability_description: String::new(),
            },
            RankedCandidate {
                entry_id: "github".into(),
                entity: "Issue".into(),
                capability_name: "issue_search".into(),
                score: 99,
                reason_codes: vec![],
                capability_description: String::new(),
            },
            RankedCandidate {
                entry_id: "linear".into(),
                entity: "Issue".into(),
                capability_name: "issue_search".into(),
                score: 98,
                reason_codes: vec![],
                capability_description: String::new(),
            },
            RankedCandidate {
                entry_id: "pokeapi".into(),
                entity: "Pokemon".into(),
                capability_name: "pokemon_query".into(),
                score: 97,
                reason_codes: vec![],
                capability_description: String::new(),
            },
        ]);
        let formatted = format_discovery_markdown_for_mcp(
            &r,
            &DiscoveryTablePolicy {
                mode: DiscoveryTableMode::PerEntryFairShare,
                max_rows: 3,
                max_per_entry: Some(8),
            },
        );
        let lines: Vec<_> = formatted
            .markdown
            .lines()
            .filter(|l| l.contains('\t') && !l.starts_with("api\t"))
            .collect();
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().any(|l| l.starts_with("github\t")));
        assert!(lines.iter().any(|l| l.starts_with("linear\t")));
        assert!(lines.iter().any(|l| l.starts_with("pokeapi\t")));
    }
}
