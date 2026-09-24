//! Row-to-text template compile helpers (column token inference and field-list resolution).

use std::collections::{BTreeMap, BTreeSet};

use crate::execute_session::ExecuteSession;
use crate::plasm_plan::{OutputName, QualifiedEntityKey};
use crate::plasm_plan_run::RenderColumns;
use plasm_core::expr_parser::{
    normalize_nested_projection_field, split_top_level, validate_program_label,
};
use plasm_core::SymbolMapCrossRequestCache;

/// Minijinja identifiers that iterate over engine builtins / globals, not render-source bindings.
/// Loop iterables rooted at these must never be treated as required render sources.
const TEMPLATE_ITERABLE_BUILTINS: &[&str] = plasm_core::MINIJINJA_TEMPLATE_BUILTINS;

/// Locals introduced by `{% for … %}` / `{% set … %}` inside a row-to-text template.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TemplateLocals {
    /// Every name bound by a `for` target or `set` statement (loop cursors, unpacked tuples, set vars).
    pub names: BTreeSet<String>,
    /// Loop cursor → the bare-binding root identifier of the iterable it ranges over
    /// (builtins are recorded so cursor field access can be attributed correctly).
    pub cursor_iterables: BTreeMap<String, String>,
    /// Bare-binding roots iterated by a `for` loop that are NOT builtins — these must be in scope.
    pub loop_iterable_roots: BTreeSet<String>,
}

/// Extract the leading bare-binding identifier of an iterable expression.
///
/// Returns `None` for literals (`[…]`, `{…}`, quotes, digits) and function calls (`range(…)`),
/// so only plain binding references (`all_labels`, `sorted | reverse`, `items[1:]`) qualify.
fn iterable_binding_root(raw: &str) -> Option<String> {
    let s = raw.trim();
    let first = s.chars().next()?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return None;
    }
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    let ident = &s[..end];
    if s[end..].trim_start().starts_with('(') {
        return None;
    }
    Some(ident.to_string())
}

/// Parse `for` targets (`x`, `x, y`, `(x, y)`) into individual binding names.
fn parse_for_targets(raw: &str) -> Vec<String> {
    raw.trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty() && validate_program_label(t).is_ok())
        .map(str::to_string)
        .collect()
}

/// Scan `{% for … %}` / `{% set … %}` statement blocks for template-local names.
pub(crate) fn infer_template_locals(template: &str) -> TemplateLocals {
    let mut out = TemplateLocals::default();
    let mut rest = template;
    while let Some(start) = rest.find("{%") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("%}") else {
            break;
        };
        let stmt = after[..end]
            .trim()
            .trim_start_matches('-')
            .trim_end_matches('-')
            .trim();
        if let Some(body) = stmt.strip_prefix("for ") {
            // `<targets> in <iterable> [if <cond>] [recursive]`
            if let Some((targets_raw, iter_raw)) = split_for_in(body) {
                let iterable_root = iterable_binding_root(iter_raw);
                for target in parse_for_targets(targets_raw) {
                    if let Some(root) = &iterable_root {
                        out.cursor_iterables.insert(target.clone(), root.clone());
                    }
                    out.names.insert(target);
                }
                if let Some(root) = iterable_root {
                    if !TEMPLATE_ITERABLE_BUILTINS.contains(&root.as_str()) {
                        out.loop_iterable_roots.insert(root);
                    }
                }
            }
        } else if let Some(body) = stmt.strip_prefix("set ") {
            if let Some(lhs) = body.split('=').next() {
                let root = lhs
                    .trim()
                    .split(['.', '[', ' ', '\t'])
                    .next()
                    .unwrap_or("")
                    .trim();
                if !root.is_empty() && validate_program_label(root).is_ok() {
                    out.names.insert(root.to_string());
                }
            }
        }
        rest = &after[end + 2..];
    }
    out
}

/// Split a `for` statement body at the top-level ` in ` keyword into `(targets, iterable)`,
/// dropping any trailing ` if <cond>` filter or ` recursive` clause from the iterable.
fn split_for_in(body: &str) -> Option<(&str, &str)> {
    let (targets, iter_and_cond) = body.split_once(" in ")?;
    let mut iter = iter_and_cond
        .split(" if ")
        .next()
        .unwrap_or(iter_and_cond)
        .trim();
    if let Some(stripped) = iter.strip_suffix(" recursive") {
        iter = stripped.trim();
    }
    Some((targets.trim(), iter))
}

/// Parsed Minijinja field references from a row-to-text template body.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct TemplateFieldRefs {
    /// Fields accessed via a genuine named-binding `{{ label.field }}`, keyed by binding label.
    pub label_fields: BTreeMap<String, Vec<String>>,
    /// Fields accessed via a loop cursor, keyed by the render-source root the cursor iterates.
    pub cursor_fields: BTreeMap<String, Vec<String>>,
    /// Template-local names (`for` targets + `set` vars) — never treated as cross-bindings.
    pub locals: BTreeSet<String>,
    /// Bare-binding roots iterated by `for` loops (excluding builtins) — must be in scope.
    pub loop_iterable_roots: BTreeSet<String>,
    /// Bare `{{ title }}` roots (not `label.field`, not locals, not builtins).
    pub bare_roots: Vec<String>,
}

fn push_unique(cols: &mut Vec<String>, field: &str) {
    if !cols.iter().any(|c| c == field) {
        cols.push(field.to_string());
    }
}

/// Scan `{{ … }}` expressions for row, cursor, and label-prefixed field references.
///
/// Loop cursors introduced by `{% for … %}` are resolved to the list they iterate, so
/// `{% for label in all_labels %}{{ label.name }}` attributes `name` to `all_labels` — it is
/// **not** a cross-binding reference to a binding named `label`.
pub(crate) fn infer_template_field_refs(template: &str) -> TemplateFieldRefs {
    let locals = infer_template_locals(template);
    let mut out = TemplateFieldRefs {
        locals: locals.names.clone(),
        loop_iterable_roots: locals.loop_iterable_roots.clone(),
        ..TemplateFieldRefs::default()
    };
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let expr = after[..end].trim();
        if let Some((label, tail)) = expr.split_once('.') {
            let label = label.trim();
            let field = first_field_segment(tail);
            if !label.is_empty()
                && !field.is_empty()
                && field != label
                && validate_program_label(label).is_ok()
            {
                if locals.names.contains(label) {
                    // Loop cursor / set local: attribute fields to the iterated source, never a binding.
                    if let Some(root) = locals.cursor_iterables.get(label).map(String::as_str) {
                        if !TEMPLATE_ITERABLE_BUILTINS.contains(&root) {
                            push_unique(
                                out.cursor_fields.entry(root.to_string()).or_default(),
                                field,
                            );
                        }
                    }
                } else {
                    push_unique(
                        out.label_fields.entry(label.to_string()).or_default(),
                        field,
                    );
                }
            }
        } else {
            let head = first_field_segment(expr);
            if !head.is_empty()
                && validate_program_label(head).is_ok()
                && !locals.names.contains(head)
                && !TEMPLATE_ITERABLE_BUILTINS.contains(&head)
            {
                push_unique(&mut out.bare_roots, head);
            }
        }
        rest = &after[end + 2..];
    }
    out
}

/// Leading `identifier` of a field-access tail, stopping at the first non-identifier rune.
///
/// `name` → `name`; `description or '—'` → `description`; `addr.city` → `addr`; `name | upper` → `name`.
fn first_field_segment(tail: &str) -> &str {
    let s = tail.trim_start();
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    &s[..end]
}

/// Classify template identifier roots as row fields vs named bindings (PLP-12).
/// A name that is both is a compile-time ambiguity error.
pub(crate) fn classify_per_row_template_names(
    template: &str,
    source_fields: &BTreeSet<String>,
    binding_names: &BTreeSet<String>,
    program_id: &str,
) -> Result<(Vec<String>, Vec<String>), String> {
    let refs = infer_template_field_refs(template);
    let mut roots: BTreeSet<String> = BTreeSet::new();
    for root in refs.bare_roots.iter().chain(refs.label_fields.keys()) {
        roots.insert(root.clone());
    }
    for root in &refs.loop_iterable_roots {
        if !refs.locals.contains(root) {
            roots.insert(root.clone());
        }
    }
    for root in plasm_core::interpolation_roots(template) {
        if !root.is_empty() {
            roots.insert(root);
        }
    }
    let mut row_fields = Vec::new();
    let mut binding_labels = Vec::new();
    for root in roots {
        if refs.locals.contains(&root) || TEMPLATE_ITERABLE_BUILTINS.contains(&root.as_str()) {
            continue;
        }
        let in_row = source_fields.contains(&root);
        let in_bind = binding_names.contains(&root);
        match (in_row, in_bind) {
            (true, true) => {
                return Err(plasm_core::plp::plp12_per_row_apply(format!(
                    "Plasm program `{program_id}`: template name `{root}` is both a row field and a program binding — rename the binding or project the field away"
                )));
            }
            (true, false) => row_fields.push(root),
            (false, true) => binding_labels.push(root),
            (false, false) => {
                return Err(plasm_core::plp::plp12_per_row_apply(format!(
                    "Plasm program `{program_id}`: template references `{root}` which is not a current-row field or an in-scope program binding"
                )));
            }
        }
    }
    Ok((row_fields, binding_labels))
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

/// When the render source is a simple in-scope binding label, expose that named binding
/// in the per-row template environment (PLP-12). A binding actually named `rows` is ordinary.
pub(crate) fn resolve_render_collection_alias(
    head_core: &str,
    columns: &[OutputName],
    label_in_scope: impl Fn(&str) -> bool,
) -> Option<OutputName> {
    let label = head_core.trim();
    if label.is_empty() || label.contains('.') {
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
        "{} `=>` applies once per row: use `{{{{ field }}}}` for the current row.",
        columns.access_hint()
    );
    if let Some(alias) = collection_alias {
        out.push_str(&format!(
            " Named binding `{alias}` remains available. Whole-collection text uses a plain template (`report = <<TAG {{% for item in {alias} %}}… TAG`)."
        ));
    }
    out
}

/// Plan JSON `depends_on` / `uses_result` edges for a render compute node.
pub(crate) fn render_plan_graph_edges(
    source: &str,
    render_bindings: &[OutputName],
) -> (Vec<String>, Vec<crate::plasm_plan::PlanResultUse>) {
    let mut depends_on = vec![source.to_string()];
    for label in render_bindings {
        let id = label.as_str();
        if id != source && !depends_on.iter().any(|d| d == id) {
            depends_on.push(id.to_string());
        }
    }
    let mut uses_result = vec![crate::plasm_plan::PlanResultUse {
        node: source.to_owned(),
        r#as: "source".to_owned(),
        qualified_entity: None,
    }];
    for label in render_bindings {
        let id = label.as_str();
        if id != source {
            uses_result.push(crate::plasm_plan::PlanResultUse {
                node: id.to_owned(),
                r#as: id.to_owned(),
                qualified_entity: None,
            });
        }
    }
    (depends_on, uses_result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_template_field_refs_collects_named_binding_fields() {
        let refs = infer_template_field_refs("cross {{ a.title }} / {{ b.owner }}");
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
    fn classify_rejects_unknown_template_name() {
        let fields = BTreeSet::from(["title".to_string()]);
        let binds = BTreeSet::from(["items".to_string()]);
        let err = classify_per_row_template_names("{{ ghost.field }}", &fields, &binds, "prog")
            .expect_err("unknown name");
        assert!(err.contains("ghost"), "{err}");
        assert!(err.contains("PLP-12"), "{err}");
    }

    #[test]
    fn classify_rejects_row_field_binding_collision() {
        let fields = BTreeSet::from(["title".to_string()]);
        let binds = BTreeSet::from(["title".to_string(), "items".to_string()]);
        let err = classify_per_row_template_names("{{ title }}", &fields, &binds, "prog")
            .expect_err("collision");
        assert!(
            err.contains("both a row field and a program binding"),
            "{err}"
        );
        assert!(err.contains("PLP-12"), "{err}");
    }

    #[test]
    fn for_loop_cursor_is_not_a_named_binding() {
        let tmpl = "{% for label in all_labels %}| {{ label.name }} | {{ label.description or '—' }} |\n{% endfor %}";
        let refs = infer_template_field_refs(tmpl);
        assert!(refs.locals.contains("label"), "label is a local: {refs:?}");
        assert!(
            refs.label_fields.is_empty(),
            "loop cursor must not be a named binding: {refs:?}"
        );
        assert_eq!(
            refs.cursor_fields.get("all_labels").map(|v| v.as_slice()),
            Some(&["name".to_string(), "description".to_string()][..]),
            "cursor fields attributed to iterated source: {refs:?}"
        );
        let fields = BTreeSet::new();
        let binds = BTreeSet::from(["all_labels".to_string()]);
        let (row, labels) = classify_per_row_template_names(tmpl, &fields, &binds, "prog")
            .expect("loop over in-scope binding");
        assert!(row.is_empty(), "{row:?}");
        assert_eq!(labels, vec!["all_labels".to_string()]);
    }

    #[test]
    fn for_loop_cursor_named_entry_is_accepted() {
        let tmpl = "{% for entry in items %}{{ entry.title }}{% endfor %}";
        let fields = BTreeSet::from(["title".to_string()]);
        let binds = BTreeSet::from(["items".to_string()]);
        let (row, labels) = classify_per_row_template_names(tmpl, &fields, &binds, "prog")
            .expect("cursor `entry` over in-scope source");
        assert!(row.is_empty(), "{row:?}");
        assert_eq!(labels, vec!["items".to_string()]);
    }

    #[test]
    fn for_loop_over_undeclared_source_is_rejected() {
        let fields = BTreeSet::new();
        let binds = BTreeSet::from(["items".to_string()]);
        let err = classify_per_row_template_names(
            "{% for x in ghost %}{{ x.name }}{% endfor %}",
            &fields,
            &binds,
            "prog",
        )
        .expect_err("iterating an out-of-scope binding must fail");
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn for_loop_over_builtin_range_is_not_treated_as_binding() {
        let fields = BTreeSet::new();
        let binds = BTreeSet::from(["items".to_string()]);
        let (row, labels) = classify_per_row_template_names(
            "{% for i in range(3) %}{{ i }}{% endfor %}",
            &fields,
            &binds,
            "prog",
        )
        .expect("range() is a builtin, not a render source");
        assert!(row.is_empty() && labels.is_empty(), "{row:?} {labels:?}");
    }

    #[test]
    fn set_local_is_not_a_named_binding() {
        let tmpl = "{% set total = 0 %}{{ total.foo }}";
        let refs = infer_template_field_refs(tmpl);
        assert!(refs.locals.contains("total"), "{refs:?}");
        assert!(refs.label_fields.is_empty(), "{refs:?}");
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
    fn render_context_hint_mentions_per_row_fields() {
        let cols =
            RenderColumns::from_field_pairs(&[("name".into(), "name".into())]).expect("cols");
        let hint = render_context_hint(&cols, Some("items"));
        assert!(
            hint.contains("{{ field }}") || hint.contains("current row"),
            "{hint}"
        );
        assert!(hint.contains("items"), "{hint}");
    }
}
