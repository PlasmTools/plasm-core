//! Row-to-text template compile helpers (column token inference and field-list resolution).

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{OutputName, QualifiedEntityKey};
use crate::plasm_plan_run::RenderColumns;
use plasm_core::expr_parser::{
    normalize_nested_projection_field, split_top_level, validate_program_label,
};
use plasm_core::SymbolMapCrossRequestCache;

/// Infer raw column tokens from `{{ r.field }}` / `{{ field }}` references in a row template body.
pub(crate) fn infer_column_tokens_from_minijinja_template(template: &str) -> Option<Vec<String>> {
    let mut cols = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let expr = after[..end].trim();
        let field = expr
            .strip_prefix("r.")
            .or_else(|| expr.strip_prefix("rows[0]."))
            .map(|f| f.split('|').next().unwrap_or(f).trim());
        let Some(field) = field else {
            rest = &after[end + 2..];
            continue;
        };
        if field == "rows" || field.is_empty() {
            rest = &after[end + 2..];
            continue;
        }
        if field
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
            && !cols.iter().any(|c| c == field)
        {
            cols.push(field.to_string());
        }
        rest = &after[end + 2..];
    }
    if cols.is_empty() {
        None
    } else {
        Some(cols)
    }
}

pub(crate) fn parse_field_list_with_tokens(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    fields: &str,
) -> Result<Vec<(String, String)>, String> {
    let out = split_top_level(fields, ',')?
        .into_iter()
        .map(|s| {
            let t = s.trim();
            normalize_nested_projection_field(t)
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|raw| {
            let wire = crate::plasm_plan_run::resolve_wire_field_token(
                session,
                symbol_map_cross_cache,
                qe,
                raw.as_str(),
            );
            (raw, wire)
        })
        .filter(|(_, wire)| !wire.is_empty())
        .collect::<Vec<_>>();
    if out.is_empty() {
        return Err("field list must be non-empty".to_string());
    }
    Ok(out)
}

/// When the render source is a simple in-scope binding label, expose the projected list under that
/// name in Minijinja (alongside `rows`).
pub(crate) fn resolve_render_collection_alias(
    head_core: &str,
    columns: &[OutputName],
    label_in_scope: impl Fn(&str) -> bool,
) -> Option<OutputName> {
    let label = head_core.trim();
    if label.is_empty() || label == "rows" || label.contains('.') {
        return None;
    }
    if !label_in_scope(label) {
        return None;
    }
    if validate_program_label(label).is_err() {
        return None;
    }
    if columns.iter().any(|c| c.as_str() == label) {
        return None;
    }
    OutputName::new(label.to_string()).ok()
}

pub(crate) fn render_context_hint(
    columns: &RenderColumns,
    collection_alias: Option<&str>,
) -> String {
    let mut out = format!(
        "{} Iterate the projected list with `{{% for r in rows %}}` (one Minijinja render over the whole list, not per-row).",
        columns.access_hint()
    );
    if let Some(alias) = collection_alias.filter(|a| *a != "rows") {
        out.push_str(&format!(
            " The same list is also bound as `{alias}` (`{{% for r in {alias} %}}`)."
        ));
    }
    out
}

pub(crate) fn resolve_inferred_render_columns(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    raw_tokens: &[String],
) -> Result<RenderColumns, String> {
    let pairs: Vec<(String, String)> = raw_tokens
        .iter()
        .map(|raw| {
            let wire = crate::plasm_plan_run::resolve_wire_field_token(
                session,
                symbol_map_cross_cache,
                qe,
                raw.as_str(),
            );
            (raw.clone(), wire)
        })
        .collect();
    RenderColumns::from_field_pairs(&pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_render_collection_alias_accepts_in_scope_label() {
        let cols =
            RenderColumns::from_field_pairs(&[("name".into(), "name".into())]).expect("cols");
        let alias =
            resolve_render_collection_alias("items", &cols.wires, |l| l == "items").expect("alias");
        assert_eq!(alias.as_str(), "items");
    }

    #[test]
    fn resolve_render_collection_alias_rejects_dotted_head_and_column_collision() {
        let cols =
            RenderColumns::from_field_pairs(&[("items".into(), "items".into())]).expect("cols");
        assert!(resolve_render_collection_alias("repo.items", &cols.wires, |_| true).is_none());
        assert!(resolve_render_collection_alias("items", &cols.wires, |_| true).is_none());
    }

    #[test]
    fn render_context_hint_mentions_rows_and_collection_alias() {
        let cols =
            RenderColumns::from_field_pairs(&[("name".into(), "name".into())]).expect("cols");
        let hint = render_context_hint(&cols, Some("items"));
        assert!(hint.contains("{% for r in rows %}"), "{hint}");
        assert!(hint.contains("{% for r in items %}"), "{hint}");
    }
}
