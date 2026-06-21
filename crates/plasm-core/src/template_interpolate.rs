//! `${binding.path}` interpolation in program string literals (heredocs, quoted strings).
//!
//! Distinct from plan-layer `PlanValue::Template` and from Minijinja row templates (`{{ }}`).

use std::collections::BTreeMap;

use crate::text::interpolate_dollar_template;
use crate::value::Value;

pub use crate::text::{DEFAULT_MAX_INTERPOLATED_LEN, InterpolateError};

/// Binding name → value for `${alias.path}` resolution.
pub type BindingScope<'a> = BTreeMap<&'a str, &'a Value>;

pub use crate::template_ref::{
    contains_dollar_interpolation, for_each_interpolation_path, interpolation_paths,
    interpolation_roots, validate_interpolation_syntax, RefKind, TemplateRefContext,
};

/// Root binding names referenced by `${name}` or `${name.path}` in `s`.
#[inline]
pub fn dollar_interpolation_roots(s: &str) -> Vec<String> {
    interpolation_roots(s)
}

/// Expand `${ident}` and `${ident.path}` using `scope` (binding roots only).
pub fn interpolate_string(
    input: &str,
    scope: &BindingScope<'_>,
) -> Result<String, InterpolateError> {
    interpolate_string_with_max(input, scope, DEFAULT_MAX_INTERPOLATED_LEN)
}

/// Like [`interpolate_string`] with an owned binding map.
pub fn interpolate_string_map(
    input: &str,
    scope: &BTreeMap<String, Value>,
) -> Result<String, InterpolateError> {
    let refs: BindingScope<'_> = scope.iter().map(|(k, v)| (k.as_str(), v)).collect();
    interpolate_string(input, &refs)
}

pub fn interpolate_string_with_max(
    input: &str,
    scope: &BindingScope<'_>,
    max_len: usize,
) -> Result<String, InterpolateError> {
    interpolate_dollar_template(input, |path| resolve_path(path, scope), max_len)
        .map(crate::text::Utf8Text::into_string)
}

fn resolve_path(path: &str, scope: &BindingScope<'_>) -> Result<String, String> {
    let path = path.trim();
    if path.is_empty() {
        return Err(list_bindings(scope));
    }
    let mut parts = path.split('.');
    let root = parts.next().unwrap();
    let Some(v) = scope.get(root) else {
        return Err(list_bindings(scope));
    };
    let mut cur = (*v).clone();
    for seg in parts {
        cur = match cur {
            Value::Object(map) => map.get(seg).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        };
    }
    scalar_to_string(&cur).ok_or_else(|| list_bindings(scope))
}

fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Null => Some(String::new()),
        _ => None,
    }
}

fn list_bindings(scope: &BindingScope<'_>) -> String {
    let mut names: Vec<_> = scope.keys().copied().collect();
    names.sort();
    if names.is_empty() {
        "(none)".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_binding_path() {
        let report = Value::Object(indexmap::IndexMap::from([(
            "content".to_string(),
            Value::String("hello".into()),
        )]));
        let scope = BindingScope::from([("spec_md", &report)]);
        let out = interpolate_string("prefix ${spec_md.content} suffix", &scope).unwrap();
        assert_eq!(out, "prefix hello suffix");
    }

    #[test]
    fn escape_dollar_dollar() {
        let scope = BindingScope::new();
        let out = interpolate_string("cost $$50", &scope).unwrap();
        assert_eq!(out, "cost $50");
    }

    #[test]
    fn unresolved_lists_bindings() {
        let binding = Value::String("x".into());
        let scope = BindingScope::from([("a", &binding)]);
        let err = interpolate_string("${missing}", &scope).unwrap_err();
        assert!(matches!(err, InterpolateError::UnresolvedReference { .. }));
    }

    #[test]
    fn preserves_utf8_literals() {
        let scope = BindingScope::new();
        let out = interpolate_string("Featured Pokémon", &scope).unwrap();
        assert_eq!(out, "Featured Pokémon");
        assert!(out.contains('é'));
    }

    #[test]
    fn preserves_utf8_with_stitch() {
        let body = Value::Object(indexmap::IndexMap::from([(
            "content".to_string(),
            Value::String("électric".into()),
        )]));
        let scope = BindingScope::from([("type_md", &body)]);
        let out = interpolate_string("# Pokémon\n${type_md.content}", &scope).unwrap();
        assert!(out.starts_with("# Pokémon"));
        assert!(out.ends_with("électric"));
    }

    #[test]
    fn template_ref_context_classifies_row_binding() {
        use crate::template_ref::{RefKind, TemplateRefContext};
        let ctx = TemplateRefContext::for_row_scope("_");
        assert_eq!(ctx.classify_root("_"), RefKind::RowBinding);
        assert!(ctx.plan_node_roots_from_string("${_.id}").is_empty());
    }
}
