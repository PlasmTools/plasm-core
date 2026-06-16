//! TSV teaching contract splitting and fence helpers.

use super::*;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// Numeric threshold used in row-compute teaching exemplars (filter/sort/limit worked examples).
pub const ROW_COMPUTE_EXEMPLAR_THRESHOLD: i64 = 300;

/// Which portion of a fenced teaching TSV block to expose on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeachingFenceSlice {
    /// `plasm_expr` / `Meaning` table only (execute waves, MCP `plasm_context`, grammar-revision cached clients, terminal).
    TableOnly,
    /// Full fenced body including optional `#` grammar contract prefix (legacy stored prompts; prefer [`Self::TableOnly`] on the wire).
    AgentFull,
}

/// SHA-256 hex digest of [`super::render_plasm_mcp_language_frontmatter`] (canonical grammar contract).
pub fn plasm_grammar_frontmatter_revision_hex() -> &'static str {
    static REVISION: OnceLock<String> = OnceLock::new();
    REVISION.get_or_init(|| {
        hex::encode(Sha256::digest(
            super::render_plasm_mcp_language_frontmatter().as_bytes(),
        ))
    })
}

/// True when the client advertises the current canonical grammar revision (case-insensitive).
pub fn client_has_cached_grammar(revision: Option<&str>) -> bool {
    revision.is_some_and(|r| r.eq_ignore_ascii_case(plasm_grammar_frontmatter_revision_hex()))
}

/// Resolve grammar revision from optional query value and `X-Plasm-Grammar-Revision` header.
pub fn grammar_revision_from_wire<'a>(
    query: Option<&'a str>,
    header: Option<&'a str>,
) -> Option<&'a str> {
    query.or(header).filter(|s| !s.is_empty())
}

/// When the client cached grammar matches [`plasm_grammar_frontmatter_revision_hex`], return table-only
/// teaching TSV (optionally re-wrapped in a markdown fence). Stored session prompts are unchanged.
pub fn teaching_prompt_omit_contract_if_cached(
    prompt: &str,
    grammar_revision: Option<&str>,
    fence_info: Option<&str>,
) -> String {
    if !client_has_cached_grammar(grammar_revision) {
        return prompt.to_string();
    }
    let table = if let Some(fence) = fence_info {
        markdown_fence_body_inner(prompt, fence)
            .map(|inner| split_tsv_teaching_contract_and_table(inner).1)
            .unwrap_or_else(|| split_tsv_teaching_contract_and_table(prompt).1)
    } else {
        split_tsv_teaching_contract_and_table(prompt).1
    };
    if let Some(fence) = fence_info {
        format!("```{fence}\n{}\n```\n", table.trim_end())
    } else {
        table
    }
}

pub fn split_tsv_teaching_contract_and_table(teaching_tsv: &str) -> (Option<String>, String) {
    if let Some(idx) = teaching_tsv.find(TSV_TEACHING_TABLE_HEADER) {
        let prefix = teaching_tsv[..idx].trim_end();
        let contract = if prefix.is_empty() {
            None
        } else {
            Some(prefix.to_string())
        };
        return (contract, teaching_tsv[idx..].to_string());
    }
    (None, teaching_tsv.to_string())
}

/// Strip a leading markdown fenced block ` ```{fence_info}\\n … \\n``` ` and return inner body.
pub fn markdown_fence_body_inner<'a>(markdown: &'a str, fence_info: &str) -> Option<&'a str> {
    let open = format!("```{fence_info}\n");
    let rest = markdown.strip_prefix(&open)?;
    let end = rest.find("\n```")?;
    Some(&rest[..end])
}

/// Extract a teaching TSV slice from a markdown-fenced session prompt.
pub fn teaching_tsv_from_wrapped_prompt(
    prompt: &str,
    fence_info: &str,
    slice: TeachingFenceSlice,
) -> Option<String> {
    let inner = markdown_fence_body_inner(prompt, fence_info)?;
    Some(match slice {
        TeachingFenceSlice::TableOnly => split_tsv_teaching_contract_and_table(inner).1,
        TeachingFenceSlice::AgentFull => inner.to_string(),
    })
}

/// teaching TSV table fragment (from [`TSV_TEACHING_TABLE_HEADER`] onward), dropping optional `#` contract lines inside the fence body.
pub fn teaching_tsv_table_from_wrapped_prompt(prompt: &str, fence_info: &str) -> Option<String> {
    teaching_tsv_from_wrapped_prompt(prompt, fence_info, TeachingFenceSlice::TableOnly)
}

/// Full fenced teaching body when stored prompts still embed a `#` contract prefix (prefer [`TeachingFenceSlice::TableOnly`] on MCP wire).
#[inline]
pub fn teaching_tsv_agent_body_from_wrapped_prompt(
    prompt: &str,
    fence_info: &str,
) -> Option<String> {
    teaching_tsv_from_wrapped_prompt(prompt, fence_info, TeachingFenceSlice::AgentFull)
}

/// Invariant for prompts emitted by [`render_prompt_tsv_from_bundle`]: from the `plasm_expr\tMeaning`
/// header through the end of the table, every non-empty body line that is not a `#` comment uses
/// **exactly one** tab between the expression column and Meaning ([`DomainTsvEncodedLine::write_line`] only;
/// middle-dot ` · ` joins gloss fragments **inside** Meaning). Tab U+0009 is emitted solely at that boundary.
pub(crate) fn validate_teaching_tsv_teaching_table(body_from_header: &str) -> Result<(), String> {
    let mut lines = body_from_header.lines();
    let header = lines
        .next()
        .ok_or_else(|| "empty teaching TSV table".to_string())?;
    let header = header.strip_suffix('\r').unwrap_or(header);
    if header != "plasm_expr\tMeaning" {
        return Err(format!(
            "expected header `plasm_expr\\tMeaning`, got {:?}",
            header.chars().take(80).collect::<String>()
        ));
    }
    for (i, raw_line) in lines.enumerate() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tabs = line.bytes().filter(|b| *b == b'\t').count();
        if tabs != 1 {
            return Err(format!(
                "line {}: expected exactly one `\\t` between `plasm_expr` and `Meaning`, got {} tab(s): {:?}",
                i + 2,
                tabs,
                line.chars().take(160).collect::<String>()
            ));
        }
        let (expr, meaning) = line.split_once('\t').expect("one tab implies split_once");
        if expr.contains('\t') || meaning.contains('\t') {
            return Err(format!(
                "line {}: stray tab inside a cell after split",
                i + 2
            ));
        }
        let expr_trim = expr.trim();
        let meaning_trim = meaning.trim();
        if expr != expr_trim {
            return Err(format!(
                "line {}: `plasm_expr` cell must not have leading/trailing whitespace (got {:?})",
                i + 2,
                expr.chars().take(120).collect::<String>()
            ));
        }
        if meaning != meaning_trim {
            return Err(format!(
                "line {}: `Meaning` cell must not have leading/trailing whitespace",
                i + 2
            ));
        }
    }
    Ok(())
}

#[inline]
pub(crate) fn enforce_teaching_tsv_teaching_invariant(prompt: &str) {
    let Some(idx) = prompt.find(TSV_TEACHING_TABLE_HEADER) else {
        return;
    };
    let body = &prompt[idx..];
    if let Err(msg) = validate_teaching_tsv_teaching_table(body) {
        tracing::error!(
            target: "plasm_core::prompt_render",
            error = %msg,
            "teaching TSV teaching table invariant violated"
        );
        debug_assert!(false, "teaching TSV: {msg}");
    }
}

/// Whether teaching table **TSV** output includes the global Plasm contract comment block.
///
/// Execute sessions (first open and additive waves) use [`Self::AdditiveWave`] — grammar is taught once
/// via MCP initialize / `plasm init`, not repeated on `plasm_context` wire responses.
/// Eval / REPL full-schema teaching uses [`Self::InitialTeaching`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DomainWaveSurface {
    /// Eval / REPL / greenfield teaching: emit global contract as leading TSV `#` comments.
    InitialTeaching,
    /// Execute-session waves (including first open): entity table only; keep `plasm_expr` / `Meaning` header.
    AdditiveWave,
}

#[cfg(test)]
mod grammar_revision_tests {
    use super::*;

    #[test]
    fn grammar_revision_hex_is_sha256_of_canonical_frontmatter() {
        let rev = plasm_grammar_frontmatter_revision_hex();
        assert_eq!(rev.len(), 64);
        assert!(client_has_cached_grammar(Some(rev)));
        assert!(!client_has_cached_grammar(Some("deadbeef")));
    }

    #[test]
    fn teaching_prompt_omit_contract_when_revision_matches() {
        let contract = super::super::render_plasm_mcp_language_frontmatter();
        let prompt = format!(
            "# {}\n\n{}e1\trow\n",
            super::super::TEACHING_VALID_EXPR_MARKER,
            super::super::TSV_TEACHING_TABLE_HEADER
        );
        assert!(prompt.contains('#'));
        let rev = plasm_grammar_frontmatter_revision_hex();
        let wire = teaching_prompt_omit_contract_if_cached(&prompt, Some(rev), None);
        assert!(!wire.contains("Grammar below"));
        assert!(wire.starts_with(super::super::TSV_TEACHING_TABLE_HEADER.trim_end()));
        assert_eq!(
            contract.len(),
            super::super::render_plasm_mcp_language_frontmatter().len()
        );
    }

    #[test]
    fn teaching_tsv_agent_body_preserves_contract() {
        let inner = format!(
            "# grammar line\n\n{}e1\trow\n",
            super::super::TSV_TEACHING_TABLE_HEADER
        );
        let fenced = format!("```tsv\n{inner}```\n");
        let body = teaching_tsv_from_wrapped_prompt(&fenced, "tsv", TeachingFenceSlice::AgentFull)
            .expect("body");
        assert!(body.contains("# grammar line"));
        assert!(body.contains("plasm_expr\tMeaning"));
        let table_only =
            teaching_tsv_from_wrapped_prompt(&fenced, "tsv", TeachingFenceSlice::TableOnly)
                .expect("table");
        assert!(!table_only.contains("# grammar line"));
        assert!(table_only.starts_with(super::super::TSV_TEACHING_TABLE_HEADER.trim_end()));
    }
}
