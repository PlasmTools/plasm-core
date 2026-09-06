//! Program-string reference scanning for Minijinja templates (post-dollar cutover).
//!
//! `${…}` is rejected. Dependency roots come from `{{ path }}` expressions.

use std::collections::HashSet;

pub use crate::program_string_template::{
    contains_dollar_interpolation, contains_minijinja_markers,
    find_dollar_interpolation_in_minijinja_body, for_each_interpolation_path, interpolation_paths,
    interpolation_roots, reject_dollar_interpolation, validate_interpolation_syntax,
};

/// How a template root should be treated during dependency collection and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// `for_each` / derive row cursor (`_` or custom `item_binding`).
    RowBinding,
    /// Cross-node input declared in `uses_result` / derive `inputs`.
    InputAlias,
    /// Not declared as an alias — often a row field when `row_binding` is set.
    Unknown,
}

/// Compile-time context for classifying Minijinja roots.
#[derive(Debug, Clone, Copy, Default)]
pub struct TemplateRefContext<'a> {
    pub row_binding: Option<&'a str>,
    pub input_aliases: &'a [(&'a str, &'a str)],
}

impl<'a> TemplateRefContext<'a> {
    #[must_use]
    pub fn for_row_scope(row_binding: &'a str) -> Self {
        Self {
            row_binding: Some(row_binding),
            input_aliases: &[],
        }
    }

    pub fn classify_root(&self, root: &str) -> RefKind {
        if self.row_binding == Some(root) {
            return RefKind::RowBinding;
        }
        if self.input_aliases.iter().any(|(alias, _)| *alias == root) {
            return RefKind::InputAlias;
        }
        RefKind::Unknown
    }

    /// Roots that should become plan-node `uses_result` edges (cross-binding inputs only).
    pub fn plan_node_roots_from_string(&self, s: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let paths = interpolation_paths(s);
        for root in interpolation_roots(s) {
            match self.classify_root(root.as_str()) {
                RefKind::RowBinding => {}
                RefKind::InputAlias => {
                    if seen.insert(root.clone()) {
                        out.push((root.clone(), root));
                    }
                }
                RefKind::Unknown => {
                    let dotted = paths.iter().any(|p| {
                        p.split_once('.')
                            .is_some_and(|(r, _)| r == root.as_str())
                    });
                    // No row cursor: every root is a cross-binding candidate.
                    // With a row cursor: bare roots are row fields; dotted roots are upstream nodes.
                    if (self.row_binding.is_none() || dotted) && seen.insert(root.clone()) {
                        out.push((root.clone(), root));
                    }
                }
            }
        }
        out
    }

    /// Validate Minijinja roots: dollar forbidden; bare Unknown names are row fields when a
    /// row cursor is in scope; dotted Unknown roots are undeclared cross-bindings.
    pub fn validate_string_roots(
        &self,
        s: &str,
        error: impl FnOnce(String) -> String,
    ) -> Result<(), String> {
        if let Err(e) = reject_dollar_interpolation(s) {
            return Err(error(e.to_string()));
        }
        let paths = interpolation_paths(s);
        for root in interpolation_roots(s) {
            match self.classify_root(root.as_str()) {
                RefKind::RowBinding | RefKind::InputAlias => {}
                RefKind::Unknown => {
                    let dotted = paths.iter().any(|p| {
                        p.split_once('.')
                            .is_some_and(|(r, _)| r == root.as_str())
                    });
                    if self.row_binding.is_some() && !dotted {
                        continue;
                    }
                    return Err(error(root));
                }
            }
            if root == "_" && self.row_binding != Some("_") {
                let cursor = self.row_binding.unwrap_or("_");
                return Err(error(format!("_ (use {cursor}.path for the row cursor)")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_roots_from_minijinja() {
        assert_eq!(
            interpolation_roots("a {{ x }} {{ y.z }}"),
            vec!["x".to_string(), "y".to_string()]
        );
    }

    #[test]
    fn classify_row_and_input() {
        let ctx = TemplateRefContext {
            row_binding: Some("_"),
            input_aliases: &[("stats", "stats_node")],
        };
        assert_eq!(ctx.classify_root("_"), RefKind::RowBinding);
        assert_eq!(ctx.classify_root("stats"), RefKind::InputAlias);
        assert_eq!(ctx.classify_root("missing"), RefKind::Unknown);
    }

    #[test]
    fn plan_node_roots_skip_row_fields_when_bound() {
        let ctx = TemplateRefContext {
            row_binding: Some("_"),
            input_aliases: &[("stats", "stats_node")],
        };
        let roots = ctx.plan_node_roots_from_string("title {{ _.id }} body {{ stats.content }}");
        assert_eq!(roots, vec![("stats".to_string(), "stats".to_string())]);
    }

    #[test]
    fn validate_rejects_dotted_unknown_cross_binding() {
        let ctx = TemplateRefContext::for_row_scope("_");
        assert!(ctx
            .validate_string_roots("{{ missing.content }}", |r| format!("undeclared alias {r}"))
            .unwrap_err()
            .contains("missing"));
    }

    #[test]
    fn validate_allows_row_fields() {
        let ctx = TemplateRefContext::for_row_scope("_");
        assert!(ctx
            .validate_string_roots("{{ title }} — {{ code }}", |r| r)
            .is_ok());
    }

    #[test]
    fn validate_rejects_dollar() {
        let ctx = TemplateRefContext::for_row_scope("_");
        let err = ctx
            .validate_string_roots("${title}", |r| r)
            .unwrap_err();
        assert!(err.contains("abolished"));
    }

    #[test]
    fn markers_detect() {
        assert!(contains_minijinja_markers("{{ a }}"));
        assert!(!contains_minijinja_markers("plain"));
        assert!(contains_dollar_interpolation("${a}"));
    }
}
