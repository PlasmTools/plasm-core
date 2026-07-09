//! Capability legend parsing for teaching TSV rows.

use super::input_legend::{CapabilityInputLegend, RowContractLegend};
use super::tsv_emit::parse_trailing_projection_bracket;
use super::TeachingExprLine;

pub(crate) const LEGEND_EM_DESC_SEP: &str = " — ";
pub(crate) const PROJECTION_WITNESS_LEGEND_MARK: &str = "· projection";

/// Capability sig / human prose tail after result gloss — shared when assembling [`TeachingExprLine`] tails.
pub(crate) fn apply_compact_legend_remainder(row: &mut TeachingExprLine, remainder: &str) {
    let (sig_part, desc_tail) = split_sig_and_human_description(remainder);
    let (sig_wo, compact) = split_compact_args_from_sig_fragment(sig_part);
    row.legend.compact_args = compact;
    let mut orphan = String::new();
    fill_scope_optional_from_sig(
        &sig_wo,
        &mut row.legend.scope,
        &mut row.legend.optional_params,
        &mut orphan,
    );
    if !desc_tail.is_empty() {
        row.legend.description = desc_tail.to_string();
        if !orphan.is_empty() {
            row.legend.description = format!("{orphan} {}", row.legend.description)
                .trim()
                .to_string();
        }
    } else if !orphan.is_empty() {
        row.legend.description = orphan;
    }
}

/// Build [`TeachingExprLine`] from structured gloss layers (model → row; no compact `;;` wire).
pub(crate) fn teaching_expr_line_from_layers(
    expr: &str,
    result_gloss: Option<&str>,
    cap_legend: Option<&str>,
    row_contract: RowContractLegend,
) -> TeachingExprLine {
    let expr = expr.trim().to_string();
    let gloss = result_gloss.map(str::trim).filter(|s| !s.is_empty());
    let cap = cap_legend.map(str::trim).filter(|s| !s.is_empty());
    let legend_present = gloss.is_some() || cap.is_some();
    if !legend_present {
        return TeachingExprLine {
            expression: expr,
            row_contract,
            ..TeachingExprLine::empty_legend(String::new())
        };
    }
    // Arrow is assigned by the push pipeline from the validated domain-line kind; default here.
    let is_projection_teaching = gloss.is_some_and(|g| g.contains(PROJECTION_WITNESS_LEGEND_MARK))
        && parse_trailing_projection_bracket(expr.trim()).is_some();
    let mut row = TeachingExprLine {
        expression: expr,
        result_type: gloss.map(|s| s.to_string()).unwrap_or_default(),
        legend: CapabilityInputLegend::default(),
        is_projection_teaching,
        row_contract,
        arrow: super::ReturnArrow::Single,
    };
    apply_compact_legend_remainder(&mut row, cap.unwrap_or(""));
    row
}

/// Compact `p#` Meaning when the slot shares a `values:` row.
///
/// Registry-backed slot Meaning for teaching-table gloss rows.
///
/// **Wire-first keys** (`owner`, `id`, …): omit when the shared `v#` row already carries type;
/// emit **point-of-use prose only** when it adds information beyond the value domain.
/// **Opaque `p#` keys** keep `v# · wire` (and optional prose).
/// Returns `(scope_line, rest)` when `sig` begins with a `[scope …]` block; otherwise `("", sig)`.
pub(crate) fn split_leading_scope_legend(sig: &str) -> (&str, &str) {
    let t = sig.trim_start();
    if !t.starts_with("[scope ") {
        return ("", sig);
    }
    let Some(end) = t.find(']') else {
        return ("", sig);
    };
    let scope_line = t[..=end].trim();
    let rest = t[end + 1..].trim_start();
    (scope_line, rest)
}

/// Split capability signature (scope / optional params) from trailing human gloss after em dash.
pub(crate) fn split_sig_and_human_description(remainder: &str) -> (&str, &str) {
    remainder
        .trim()
        .split_once(LEGEND_EM_DESC_SEP)
        .map(|(a, b)| (a.trim(), b.trim()))
        .unwrap_or((remainder.trim(), ""))
}

/// Strip `args: …` (and its leading ` · ` joiner) from a capability sig fragment; remainder goes to
/// scope/optional parsing, body is the compact slot summary for TSV `Meaning` parity.
pub(crate) fn split_compact_args_from_sig_fragment(sig: &str) -> (String, String) {
    let t = sig.trim();
    if let Some(idx) = t.rfind(" · args:") {
        let a = t[..idx].trim();
        let b = t[idx + " · args:".len()..].trim();
        return (a.to_string(), b.to_string());
    }
    if let Some(s) = t.strip_prefix("args:") {
        return (String::new(), s.trim().to_string());
    }
    (t.to_string(), String::new())
}

/// Meaning-column `optional` is only for rows that teach optional invoke/query slots in the
/// expression (`p#=$` placeholders or `,..` / `(..)` ellipsis). Zero-arity invokes like `.m33()`
/// must not carry the legend when the schema's optional params are omitted from the exemplar.
pub(crate) fn teaching_expr_demonstrates_optional_params(
    expr: &str,
    optional_syms: &[String],
) -> bool {
    if optional_syms.is_empty() {
        return false;
    }
    if expr.contains(",..") || expr.contains("(..)") {
        return true;
    }
    optional_syms
        .iter()
        .any(|sym| !sym.is_empty() && expr.contains(sym.as_str()))
}

pub(crate) fn fill_scope_optional_from_sig(
    sig: &str,
    scope: &mut String,
    optional_params: &mut Vec<String>,
    orphan: &mut String,
) {
    scope.clear();
    optional_params.clear();
    orphan.clear();
    let (sc, after_sc) = split_leading_scope_legend(sig);
    *scope = sc.to_string();
    let tail = after_sc.trim();
    if let Some(p) = tail
        .strip_prefix("optional params:")
        .or_else(|| tail.strip_prefix("opt:"))
        .or_else(|| tail.strip_prefix("optional:"))
    {
        let list = p.trim();
        if !list.is_empty() {
            *optional_params = list
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
        }
    } else if !tail.is_empty() {
        *orphan = tail.to_string();
    }
}
