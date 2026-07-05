//! Row-to-text template compile helpers (column token inference and field-list resolution).

use std::collections::BTreeMap;

use serde_json::json;

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{OutputName, QualifiedEntityKey};
use crate::plasm_plan_run::RenderColumns;
use plasm_core::expr_parser::{
    normalize_nested_projection_field, split_top_level, validate_program_label,
};
use plasm_core::SymbolMapCrossRequestCache;

/// Parsed Minijinja field references from a row-to-text template body.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TemplateFieldRefs {
    /// Fields accessed via `{{ r.field }}` or `{{ rows[0].field }}`.
    pub row_fields: Vec<String>,
    /// Fields accessed via `{{ label.field }}`, keyed by binding label.
    pub label_fields: BTreeMap<String, Vec<String>>,
}

/// Scan `{{ … }}` expressions for row and label-prefixed field references.
pub(crate) fn infer_template_field_refs(template: &str) -> TemplateFieldRefs {
    let mut out = TemplateFieldRefs::default();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let expr = after[..end].trim();
        if let Some(field) = expr
            .strip_prefix("r.")
            .or_else(|| expr.strip_prefix("rows[0]."))
        {
            let field = field
                .split('|')
                .next()
                .unwrap_or(field)
                .trim()
                .split('.')
                .next()
                .unwrap_or(field)
                .trim();
            if !field.is_empty()
                && field != "rows"
                && field
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                && !out.row_fields.iter().any(|c| c == field)
            {
                out.row_fields.push(field.to_string());
            }
        } else if let Some((label, tail)) = expr.split_once('.') {
            let label = label.trim();
            let field = tail
                .split('|')
                .next()
                .unwrap_or(tail)
                .trim()
                .split('.')
                .next()
                .unwrap_or(tail)
                .trim();
            if !label.is_empty()
                && !field.is_empty()
                && field != label
                && validate_program_label(label).is_ok()
                && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                let cols = out.label_fields.entry(label.to_string()).or_default();
                if !cols.iter().any(|c| c == field) {
                    cols.push(field.to_string());
                }
            }
        }
        rest = &after[end + 2..];
    }
    out
}

/// Infer wire column tokens for render projection from a template body.
pub(crate) fn infer_render_column_tokens_from_template(
    template: &str,
    primary_label: &str,
) -> Option<Vec<String>> {
    let refs = infer_template_field_refs(template);
    if !refs.row_fields.is_empty() {
        return Some(refs.row_fields);
    }
    refs.label_fields
        .get(primary_label)
        .cloned()
        .filter(|cols| !cols.is_empty())
}

/// Ensure every `{{ label.field }}` reference uses an in-scope render source label.
pub(crate) fn validate_template_binding_labels(
    template: &str,
    allowed_labels: &[String],
    program_id: &str,
) -> Result<(), String> {
    let refs = infer_template_field_refs(template);
    for label in refs.label_fields.keys() {
        if !allowed_labels.iter().any(|allowed| allowed == label) {
            return Err(format!(
                "Plasm program `{program_id}`: template references binding `{label}` which is not among render sources {:?}",
                allowed_labels
            ));
        }
    }
    Ok(())
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
            )?;
            Ok::<(String, String), String>((raw, wire))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let out: Vec<(String, String)> = out
        .into_iter()
        .filter(|(_, wire)| !wire.is_empty())
        .collect();
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
            )?;
            Ok::<(String, String), String>((raw.clone(), wire))
        })
        .collect::<Result<Vec<_>, _>>()?;
    RenderColumns::from_field_pairs(&pairs)
}

/// Plan JSON `depends_on` / `uses_result` edges for a render compute node.
pub(crate) fn render_plan_graph_edges(
    source: &str,
    render_bindings: &[OutputName],
) -> (Vec<String>, Vec<serde_json::Value>) {
    let mut depends_on = vec![source.to_string()];
    for label in render_bindings {
        let id = label.as_str();
        if id != source && !depends_on.iter().any(|d| d == id) {
            depends_on.push(id.to_string());
        }
    }
    let mut uses_result = vec![serde_json::json!({ "node": source, "as": "source" })];
    for label in render_bindings {
        let id = label.as_str();
        if id != source {
            uses_result.push(json!({ "node": id, "as": id }));
        }
    }
    (depends_on, uses_result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_template_field_refs_collects_row_and_label_fields() {
        let refs =
            infer_template_field_refs("Row {{ r.name }} cross {{ a.title }} / {{ b.owner }}");
        assert_eq!(refs.row_fields, vec!["name".to_string()]);
        assert_eq!(
            refs.label_fields.get("a").map(|v| v.as_slice()),
            Some(&["title".to_string()][..])
        );
        assert_eq!(
            refs.label_fields.get("b").map(|v| v.as_slice()),
            Some(&["owner".to_string()][..])
        );
    }

    #[test]
    fn validate_template_binding_labels_rejects_unknown_labels() {
        let err = validate_template_binding_labels(
            "{{ ghost.field }}",
            &["a".to_string(), "b".to_string()],
            "prog",
        )
        .expect_err("unknown label");
        assert!(err.contains("ghost"), "{err}");
    }

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
