//! Unified `${binding.path}` reference scanning and compile-time classification.
//!
//! Single scanner for program string literals, plan derive templates, effect IR strings,
//! and runtime interpolation. Distinct from Minijinja row templates (`{{ }}`).

use std::collections::HashSet;

/// How a `${…}` root should be treated during dependency collection and validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    /// `for_each` / derive row cursor (`_` or custom `item_binding`).
    RowBinding,
    /// Cross-node input declared in `uses_result` / derive `inputs`.
    InputAlias,
    /// Not declared in the current template context.
    Unknown,
}

/// Compile-time context for classifying `${root}` / `${root.path}` references.
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
        for root in interpolation_roots(s) {
            match self.classify_root(root.as_str()) {
                RefKind::RowBinding => {}
                RefKind::InputAlias | RefKind::Unknown => {
                    if seen.insert(root.clone()) {
                        out.push((root.clone(), root));
                    }
                }
            }
        }
        out
    }

    /// Validate every `${…}` root in `s` against row binding + declared input aliases.
    pub fn validate_string_roots(
        &self,
        s: &str,
        error: impl FnOnce(String) -> String,
    ) -> Result<(), String> {
        for root in interpolation_roots(s) {
            if self.classify_root(root.as_str()) == RefKind::Unknown {
                return Err(error(root));
            }
            if root == "_" && self.row_binding != Some("_") {
                let cursor = self.row_binding.unwrap_or("_");
                return Err(error(format!("_ (use {cursor}.path for the row cursor)")));
            }
        }
        Ok(())
    }
}

/// Returns true if `s` contains a `${` interpolation opener (not `$$`).
pub fn contains_dollar_interpolation(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$' {
            if bytes[i + 1] == b'$' {
                i += 2;
                continue;
            }
            if bytes[i + 1] == b'{' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Full trimmed paths inside `${…}` (e.g. `_.p34`, `stats.content`). Respects `$$` escape.
pub fn interpolation_paths(s: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for_each_interpolation_path(s, |path| paths.push(path.to_string()));
    paths
}

/// Root binding names referenced by `${name}` or `${name.path}` in `s`.
pub fn interpolation_roots(s: &str) -> Vec<String> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for_each_interpolation_path(s, |path| {
        if let Some(root) = path.split('.').next() {
            if !root.is_empty() && seen.insert(root.to_string()) {
                roots.push(root.to_string());
            }
        }
    });
    roots
}

/// Invoke `f` with each `${…}` path (trimmed). Skips `$$` escapes.
pub fn for_each_interpolation_path<F: FnMut(&str)>(s: &str, mut f: F) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'$' {
            if bytes[i + 1] == b'$' {
                i += 2;
                continue;
            }
            if bytes[i + 1] == b'{' {
                let start = i + 2;
                let Some(end_rel) = s[start..].find('}') else {
                    i += 1;
                    continue;
                };
                f(s[start..start + end_rel].trim());
                i = start + end_rel + 1;
                continue;
            }
        }
        i += 1;
    }
}

/// First `${…}` span in a Minijinja row-template body, skipping `{% raw %}…{% endraw %}` and `$$`.
pub fn find_dollar_interpolation_in_minijinja_body(s: &str) -> Option<String> {
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with("{% raw %}") {
            if let Some(rel) = s[i..].find("{% endraw %}") {
                i += rel + "{% endraw %}".len();
                continue;
            }
        }
        let Some(rest) = s.get(i..) else {
            break;
        };
        if rest.starts_with("$$") {
            i += 2;
            continue;
        }
        if rest.starts_with("${") {
            let start = i;
            if let Some(rel) = rest.find('}') {
                i += rel + 1;
                return Some(s[start..i].to_string());
            }
            return Some(s[start..].to_string());
        }
        i += rest.chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    None
}

/// Syntax-only validation: balanced `${…}`, non-empty paths.
pub fn validate_interpolation_syntax(
    s: &str,
    error: impl Fn(String) -> String,
) -> Result<(), String> {
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(error("contains an unterminated ${...} substitution".into()));
        };
        if after[..end].trim().is_empty() {
            return Err(error("contains an empty ${...} substitution".into()));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolation_roots_skips_dollar_escape() {
        assert_eq!(interpolation_roots("a ${x} $${y}"), vec!["x"]);
        assert_eq!(interpolation_roots("a ${x} $$50"), vec!["x"]);
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
    fn plan_node_roots_skip_row_binding() {
        let ctx = TemplateRefContext::for_row_scope("_");
        let roots = ctx.plan_node_roots_from_string("title ${_.id} body ${stats.content}");
        assert_eq!(roots, vec![("stats".to_string(), "stats".to_string())]);
    }

    #[test]
    fn minijinja_body_dollar_scan_respects_raw_and_escape() {
        assert_eq!(
            find_dollar_interpolation_in_minijinja_body("${report.content}"),
            Some("${report.content}".into())
        );
        assert!(find_dollar_interpolation_in_minijinja_body("{% raw %}${x}{% endraw %}").is_none());
        assert!(find_dollar_interpolation_in_minijinja_body("$$ literal").is_none());
        assert!(find_dollar_interpolation_in_minijinja_body("score: {{ r.p9 or \"—\" }}").is_none());
    }
}
