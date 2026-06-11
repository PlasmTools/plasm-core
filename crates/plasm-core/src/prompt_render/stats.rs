//! Prompt surface statistics.

use super::*;
use std::collections::BTreeMap;

/// Byte breakdown of the grammar contract preamble vs teaching table body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrammarFrontmatterStats {
    /// Raw contract bytes (comment prefix stripped when sourced from TSV `#` lines).
    pub contract_bytes: usize,
    /// Contract bytes as embedded in first-wave TSV (`#`-prefixed comment block).
    pub contract_comment_bytes: usize,
    /// Bytes from `plasm_expr\\tMeaning` header through end of prompt.
    pub table_bytes: usize,
    /// Full rendered prompt bytes (contract comment block + table when present).
    pub total_prompt_bytes: usize,
    /// Per-section byte counts on the raw (uncommented) contract text.
    pub section_bytes: BTreeMap<&'static str, usize>,
}

impl GrammarFrontmatterStats {
    /// Human-readable contract vs table split for CLI stderr (`dump_prompt`, eval).
    pub fn summary_line_body(&self) -> String {
        let contract_pct = if self.total_prompt_bytes == 0 {
            0.0
        } else {
            100.0 * self.contract_comment_bytes as f64 / self.total_prompt_bytes as f64
        };
        let mut sections = String::new();
        for (name, bytes) in &self.section_bytes {
            if !sections.is_empty() {
                sections.push(' ');
            }
            let _ = write!(sections, "{name}={bytes}");
        }
        format!(
            "contract: {} B ({contract_pct:.1}%) | table: {} B | sections: {sections}",
            self.contract_comment_bytes, self.table_bytes,
        )
    }
}

const GRAMMAR_SECTION_MARKERS: &[(&str, &str)] = &[
    ("output", "Output:"),
    ("tsv_semantics", "TSV table semantics:"),
    ("symbol_rules", "Symbol and fill rules:"),
    ("grammar", "Grammar:"),
    ("composition", "Composition rules:"),
    ("pitfalls", "Common pitfalls:"),
];

/// Strip leading `#` / `# ` comment prefixes from a first-wave TSV contract block.
pub fn strip_tsv_comment_contract_prefix(comment_block: &str) -> String {
    let mut out = String::new();
    for (i, line) in comment_block.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if line == "#" {
            continue;
        }
        if let Some(rest) = line.strip_prefix("# ") {
            out.push_str(rest);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// Section byte map for raw (uncommented) grammar contract text.
pub fn grammar_frontmatter_section_bytes(contract: &str) -> BTreeMap<&'static str, usize> {
    let mut map = BTreeMap::new();
    if let Some(output_idx) = contract.find("Output:") {
        let opener = contract[..output_idx].trim_end();
        if !opener.is_empty() {
            map.insert("opener", opener.len());
        }
    }
    for (i, (name, marker)) in GRAMMAR_SECTION_MARKERS.iter().enumerate() {
        let Some(start) = contract.find(marker) else {
            continue;
        };
        let end = GRAMMAR_SECTION_MARKERS
            .get(i + 1)
            .and_then(|(_, next_marker)| contract.find(next_marker))
            .unwrap_or(contract.len());
        let body = contract[start..end].trim_end();
        map.insert(*name, body.len());
    }
    map
}

/// Stats for canonical grammar frontmatter (no teaching table).
pub fn grammar_frontmatter_stats_from_contract(contract: &str) -> GrammarFrontmatterStats {
    let contract_bytes = contract.len();
    GrammarFrontmatterStats {
        contract_bytes,
        contract_comment_bytes: contract_bytes,
        table_bytes: 0,
        total_prompt_bytes: contract_bytes,
        section_bytes: grammar_frontmatter_section_bytes(contract),
    }
}

/// Stats for a rendered teaching TSV prompt (optional `#` contract + table).
pub fn grammar_frontmatter_stats_from_prompt(prompt: &str) -> GrammarFrontmatterStats {
    let (contract_comment, table) = split_tsv_teaching_contract_and_table(prompt);
    let contract_comment_bytes = contract_comment.as_ref().map(|s| s.len()).unwrap_or(0);
    let raw_contract = contract_comment
        .as_deref()
        .map(strip_tsv_comment_contract_prefix)
        .unwrap_or_default();
    let contract_bytes = raw_contract.len();
    let table_bytes = table.len();
    GrammarFrontmatterStats {
        contract_bytes,
        contract_comment_bytes,
        table_bytes,
        total_prompt_bytes: prompt.len(),
        section_bytes: grammar_frontmatter_section_bytes(&raw_contract),
    }
}

pub fn json_tool_surface_counts(
    cgs: &CGS,
    focus: FocusSpec<'_>,
    symbol_tuning: bool,
) -> (usize, usize) {
    let (names, _) =
        crate::symbol_tuning::resolve_prompt_surface_entities(cgs, focus, symbol_tuning);
    cap_nav_counts_from_names(cgs, &names)
}

fn cap_nav_counts_from_names(cgs: &CGS, names: &[String]) -> (usize, usize) {
    let full_set: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
    let capability_tools = cgs
        .capabilities
        .values()
        .filter(|cap| full_set.contains(cap.domain.as_str()))
        .count();
    let mut navigation_tools = 0usize;
    for e in names {
        if let Some(ent) = cgs.get_entity(e.as_str()) {
            navigation_tools += navigation_edge_count(cgs, ent);
        }
    }
    (capability_tools, navigation_tools)
}

pub(crate) fn domain_expression_tool_count_resolved(
    cgs: &CGS,
    names: &[String],
    exposure_opt: Option<&crate::symbol_tuning::TeachingExposureSession>,
    symbol_tuning: bool,
) -> usize {
    let full_entities: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let map: Option<Arc<crate::symbol_tuning::SymbolMap>> = if symbol_tuning {
        exposure_opt.map(|e| e.symbol_map_arc())
    } else {
        Some(Arc::new(crate::symbol_tuning::SymbolMap::build(
            cgs,
            &full_entities,
        )))
    };
    let mut n = 0usize;
    let mut line_valid_cache = HashMap::new();
    let line_valid_cache_seed = exposure_opt
        .map(prompt_line_valid_cache_seed_exposure)
        .unwrap_or_else(|| prompt_line_valid_cache_seed_cgs(cgs));
    let surface_filter = exposure_opt.map(|e| &e.surface);
    let entity_catalog_ids: IndexMap<(&str, &str), ()> = exposure_opt
        .map(exposure_qualified_catalog_ids)
        .unwrap_or_default();
    for &ename in &full_entities {
        let mut seen_expr: HashSet<TeachingRowDedupeKey> = HashSet::new();
        let mut gloss_emit_none = None;
        let session_entry_id = catalog_entry_id_for_exposed_entity(&entity_catalog_ids, ename)
            .map(str::to_string)
            .or_else(|| cgs.entry_id.clone());
        let block = collect_entity_teaching_block(
            cgs,
            ename,
            map.as_deref(),
            None,
            false,
            &mut line_valid_cache,
            line_valid_cache_seed,
            map.clone(),
            &mut gloss_emit_none,
            surface_filter,
            session_entry_id.as_deref(),
        );
        for row in &block.teaching_rows {
            if seen_expr.insert(row.dedupe_key.clone()) {
                n += 1;
            }
        }
    }
    n
}

/// Full stats for a prompt string already rendered with `config` (same `config.focus` as render).
pub fn prompt_surface_stats(
    cgs: &CGS,
    config: RenderConfig<'_>,
    prompt: &str,
) -> PromptSurfaceStats {
    let (names, exposure_opt) = crate::symbol_tuning::resolve_prompt_surface_entities(
        cgs,
        config.focus,
        config.uses_symbols(),
    );
    let (capability_tools, navigation_tools) = cap_nav_counts_from_names(cgs, &names);
    let json_tool_estimate = domain_expression_tool_count_resolved(
        cgs,
        &names,
        exposure_opt.as_ref(),
        config.uses_symbols(),
    );
    let prompt_chars = prompt.chars().count();
    let token_estimate = prompt_chars / 4;
    let prompt_tokens_o200k = crate::o200k_token_count::o200k_token_count(prompt);
    PromptSurfaceStats {
        prompt_chars,
        token_estimate,
        prompt_tokens_o200k,
        capability_tools,
        navigation_tools,
        json_tool_estimate,
    }
}

fn navigation_edge_count(cgs: &CGS, ent: &EntityDef) -> usize {
    let rel_names: HashSet<&str> = ent.relations.keys().map(|s| s.as_str()).collect();
    let mut n = ent.relations.len();
    for (fname, f) in &ent.fields {
        if f.named_value(cgs)
            .ok()
            .is_some_and(|nv| matches!(nv.field_type, FieldType::EntityRef { .. }))
            && !rel_names.contains(fname.as_str())
        {
            n += 1;
        }
    }
    n
}
