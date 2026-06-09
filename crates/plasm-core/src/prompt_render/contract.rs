//! TSV teaching contract splitting and fence helpers.

use super::*;

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

/// teaching TSV table fragment (from [`TSV_TEACHING_TABLE_HEADER`] onward), dropping optional `#` contract lines inside the fence body.
pub fn teaching_tsv_table_from_wrapped_prompt(prompt: &str, fence_info: &str) -> Option<String> {
    let inner = markdown_fence_body_inner(prompt, fence_info)?;
    Some(split_tsv_teaching_contract_and_table(inner).1)
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
/// Execute-session **additive** waves ([`crate::prompt_pipeline::PromptPipelineConfig::render_teaching_exposure_delta`])
/// must use [`Self::AdditiveWave`] so we do not re-broadcast grammar boilerplate on every capability append.
/// Full-schema / first-wave teaching uses [`Self::InitialTeaching`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DomainWaveSurface {
    /// First wave or greenfield teaching: emit global contract as leading TSV `#` comments.
    InitialTeaching,
    /// Subsequent waves: new entity rows only; keep `plasm_expr` / `Meaning` header for a self-describing fragment.
    AdditiveWave,
}
