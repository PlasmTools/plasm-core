//! Program string templates: Minijinja-only expansion (S1∪S2∪S3).
//!
//! Law:
//! - `${…}` / `$$` are abolished — hard error, no evaluation fallback.
//! - Marker-free strings (`{{` / `{%` absent) are opaque literals.
//! - Markers present → Minijinja with shared filters (incl. `split_part`).

use std::collections::{BTreeMap, HashSet};

use minijinja::{Environment, UndefinedBehavior, Value as MjValue};
use thiserror::Error;

use crate::value::Value;

pub const DEFAULT_MAX_INTERPOLATED_LEN: usize = 512 * 1024;

const DOLLAR_HARD_ERROR: &str = "abolished `${…}` / `$$` string interpolation; use Minijinja `{{ path }}` (filters: `| split_part`) or a bare wire `param=binding.content`";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProgramStringError {
    #[error("{0}")]
    DollarForbidden(String),
    #[error("template render: {0}")]
    Render(String),
    #[error("interpolated string exceeds maximum length ({max} bytes)")]
    MaxLengthExceeded { max: usize },
}

/// A string-producing program operand compiled at the source/wire boundary.
/// Literal and returned strings never acquire this type merely by containing braces.
#[derive(Clone)]
pub struct CompiledProgramString {
    source: String,
    environment: std::sync::Arc<Environment<'static>>,
    roots: std::collections::BTreeSet<String>,
    paths: Vec<Vec<String>>,
}

impl std::fmt::Debug for CompiledProgramString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("CompiledProgramString")
            .field(&self.source)
            .finish()
    }
}
impl PartialEq for CompiledProgramString {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}
impl Eq for CompiledProgramString {}

/// Explicit string-expression wire fields compile while decoding, before admission.
pub mod source_wire {
    use super::CompiledProgramString;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &CompiledProgramString,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(value.source())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<CompiledProgramString, D::Error> {
        let source = String::deserialize(deserializer)?;
        CompiledProgramString::compile(source).map_err(serde::de::Error::custom)
    }
}
impl CompiledProgramString {
    pub fn compile(source: String) -> Result<Self, ProgramStringError> {
        reject_dollar_interpolation(&source)?;
        let mut environment = program_string_env();
        environment
            .add_template_owned("operand", source.clone())
            .map_err(|error| ProgramStringError::Render(error.to_string()))?;
        let roots = environment
            .get_template("operand")
            .map_err(|error| ProgramStringError::Render(error.to_string()))?
            .undeclared_variables(false)
            .into_iter()
            .collect();
        let paths = environment
            .get_template("operand")
            .map_err(|error| ProgramStringError::Render(error.to_string()))?
            .undeclared_variables(true)
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .map(|path| path.split('.').map(str::to_owned).collect())
            .collect();
        Ok(Self {
            source,
            environment: std::sync::Arc::new(environment),
            roots,
            paths,
        })
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn paths(&self) -> &[Vec<String>] {
        &self.paths
    }
    pub fn roots(&self) -> &std::collections::BTreeSet<String> {
        &self.roots
    }
    pub fn render(&self, scope: &BTreeMap<String, Value>) -> Result<String, ProgramStringError> {
        let context: BTreeMap<_, _> = scope
            .iter()
            .map(|(key, value)| (key.as_str(), plasm_to_mj(value)))
            .collect();
        let output = self
            .environment
            .get_template("operand")
            .map_err(|error| ProgramStringError::Render(error.to_string()))?
            .render(context)
            .map_err(|error| ProgramStringError::Render(error.to_string()))?;
        if output.len() > DEFAULT_MAX_INTERPOLATED_LEN {
            return Err(ProgramStringError::MaxLengthExceeded {
                max: DEFAULT_MAX_INTERPOLATED_LEN,
            });
        }
        Ok(output)
    }
}
impl serde::Serialize for CompiledProgramString {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("__plasm_string_template", &self.source)?;
        map.end()
    }
}
impl<'de> serde::Deserialize<'de> for CompiledProgramString {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            __plasm_string_template: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::compile(wire.__plasm_string_template).map_err(serde::de::Error::custom)
    }
}

/// True when `s` contains a `${` opener (including after `$$` — dollar dialect is fully banned).
#[must_use]
pub fn contains_dollar_interpolation(s: &str) -> bool {
    s.contains("${")
}

/// True when the string opts into Minijinja evaluation.
#[must_use]
pub fn contains_minijinja_markers(s: &str) -> bool {
    s.contains("{{") || s.contains("{%")
}

/// Hard-error if `${` appears anywhere in a program string expansion surface.
pub fn reject_dollar_interpolation(s: &str) -> Result<(), ProgramStringError> {
    if let Some(idx) = s.find("${") {
        let span = s[idx..].chars().take(48).collect::<String>();
        return Err(ProgramStringError::DollarForbidden(format!(
            "{DOLLAR_HARD_ERROR} (found near {span:?})"
        )));
    }
    Ok(())
}

/// Register filters shared by program strings and CGS view computed templates.
pub fn register_shared_minijinja_filters(env: &mut Environment<'_>) {
    env.add_filter(
        "urlencode",
        |s: String| -> Result<String, minijinja::Error> {
            Ok(url::form_urlencoded::byte_serialize(s.as_bytes()).collect())
        },
    );
    env.add_filter("strip_trailing_slash", |s: String| -> String {
        s.trim_end_matches('/').to_string()
    });
    env.add_filter(
        "split",
        |s: String, sep: String| -> Result<Vec<String>, minijinja::Error> {
            if sep.is_empty() {
                return Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "split: separator must be non-empty",
                ));
            }
            Ok(s.split(&sep).map(str::to_string).collect())
        },
    );
    env.add_filter(
        "split_part",
        |s: String, sep: String, index: i64| -> Result<String, minijinja::Error> {
            if sep.is_empty() {
                return Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "split_part: separator must be non-empty",
                ));
            }
            let idx = usize::try_from(index.max(0)).unwrap_or(0);
            Ok(s.split(&sep).nth(idx).unwrap_or("").to_string())
        },
    );
}

fn program_string_env() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_auto_escape_callback(|_| minijinja::AutoEscape::None);
    register_shared_minijinja_filters(&mut env);
    env
}

fn plasm_to_mj(v: &Value) -> MjValue {
    match v {
        Value::Null => MjValue::from(()),
        Value::Bool(b) => MjValue::from(*b),
        Value::Integer(i) => MjValue::from(*i),
        Value::Float(f) => MjValue::from(*f),
        Value::String(s) | Value::PhraseIdent(s) => MjValue::from(s.as_str()),
        Value::Array(items) => {
            MjValue::from_iter(items.iter().map(plasm_to_mj).collect::<Vec<_>>())
        }
        Value::Object(map) => {
            let mut obj = BTreeMap::new();
            for (k, val) in map {
                obj.insert(k.clone(), plasm_to_mj(val));
            }
            MjValue::from_serialize(&obj)
        }
        Value::Money(m) => MjValue::from(m.display()),
        Value::StringTemplate(_) | Value::PlasmInputRef(_) | Value::UnionCtor { .. } => {
            MjValue::from(())
        }
    }
}

/// Expand a program string under `scope` (binding / row-field map).
///
/// Marker-free → clone. Dollar → error. Else Minijinja render.
pub fn render_program_string(
    input: &str,
    scope: &BTreeMap<String, Value>,
) -> Result<String, ProgramStringError> {
    render_program_string_with_max(input, scope, DEFAULT_MAX_INTERPOLATED_LEN)
}

pub fn render_program_string_with_max(
    input: &str,
    scope: &BTreeMap<String, Value>,
    max_len: usize,
) -> Result<String, ProgramStringError> {
    reject_dollar_interpolation(input)?;
    if !contains_minijinja_markers(input) {
        if input.len() > max_len {
            return Err(ProgramStringError::MaxLengthExceeded { max: max_len });
        }
        return Ok(input.to_string());
    }
    let env = program_string_env();
    let tmpl = env
        .template_from_str(input)
        .map_err(|e| ProgramStringError::Render(e.to_string()))?;
    let mut ctx = BTreeMap::new();
    for (k, v) in scope {
        ctx.insert(k.clone(), plasm_to_mj(v));
    }
    let out = tmpl
        .render(ctx)
        .map_err(|e| ProgramStringError::Render(e.to_string()))?;
    if out.len() > max_len {
        return Err(ProgramStringError::MaxLengthExceeded { max: max_len });
    }
    Ok(out)
}

/// Scan `{{ … }}` expressions for dotted paths / bare roots (dependency collection).
pub fn interpolation_paths(s: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let expr = after[..end].trim();
        // Skip statements mistaken into expr (should be rare inside {{ }})
        if !expr.is_empty() {
            let head = expr
                .split('|')
                .next()
                .unwrap_or(expr)
                .split_whitespace()
                .next()
                .unwrap_or("");
            // Strip trailing call/index: name, name.field, rows[0].field → take ident.path prefix
            let path = strip_expr_path_head(head);
            if !path.is_empty() && seen.insert(path.to_string()) {
                paths.push(path.to_string());
            }
        }
        rest = &after[end + 2..];
    }
    paths
}

fn strip_expr_path_head(expr: &str) -> &str {
    let mut out = expr;
    // Stop at filter already handled; also stop at `[` `(` for path root.field
    if let Some(i) = out.find(['[', '(', ' ', '\t']) {
        out = &out[..i];
    }
    out.trim_end_matches(['.', ')', ']'])
}

pub fn interpolation_roots(s: &str) -> Vec<String> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for path in interpolation_paths(s) {
        if let Some(root) = path.split('.').next() {
            if !root.is_empty() && seen.insert(root.to_string()) {
                roots.push(root.to_string());
            }
        }
    }
    roots
}

/// First `${…}` span — used only to hard-reject dollar in Minijinja bodies.
pub fn find_dollar_interpolation_in_minijinja_body(s: &str) -> Option<String> {
    s.find("${").map(|i| s[i..].chars().take(48).collect())
}

/// Invoke `f` with each Minijinja path (trimmed).
pub fn for_each_interpolation_path<F: FnMut(&str)>(s: &str, mut f: F) {
    for path in interpolation_paths(s) {
        f(&path);
    }
}

/// Reject dollar; parse-check Minijinja when markers are present.
pub fn validate_interpolation_syntax(
    s: &str,
    error: impl Fn(String) -> String,
) -> Result<(), String> {
    if let Err(e) = reject_dollar_interpolation(s) {
        return Err(error(e.to_string()));
    }
    if !contains_minijinja_markers(s) {
        return Ok(());
    }
    let env = program_string_env();
    env.template_from_str(s)
        .map(|_| ())
        .map_err(|e| error(format!("invalid Minijinja template: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn scope_title() -> BTreeMap<String, Value> {
        let mut row = IndexMap::new();
        row.insert("title".into(), Value::String("Hello".into()));
        row.insert("code".into(), Value::String("AB".into()));
        let mut scope = BTreeMap::new();
        scope.insert("_".into(), Value::Object(row.clone()));
        scope.insert("title".into(), Value::String("Hello".into()));
        scope.insert("code".into(), Value::String("AB".into()));
        let mut report = IndexMap::new();
        report.insert("content".into(), Value::String("BODY".into()));
        scope.insert("report".into(), Value::Object(report));
        scope
    }

    #[test]
    fn marker_free_passthrough() {
        let out = render_program_string("plain text $5", &BTreeMap::new()).unwrap();
        assert_eq!(out, "plain text $5");
    }

    #[test]
    fn dollar_hard_errors() {
        let err = render_program_string("${title}", &scope_title()).unwrap_err();
        assert!(matches!(err, ProgramStringError::DollarForbidden(_)));
    }

    #[test]
    fn minijinja_row_and_content() {
        let out =
            render_program_string("{{ title }} / {{ report.content }}", &scope_title()).unwrap();
        assert_eq!(out, "Hello / BODY");
    }

    #[test]
    fn split_part_filter() {
        let mut scope = BTreeMap::new();
        scope.insert("blob".into(), Value::String("left:right".into()));
        let out = render_program_string(
            "{{ blob | split_part(':', 1) }}/{{ blob | split_part(':', 0) }}",
            &scope,
        )
        .unwrap();
        assert_eq!(out, "right/left");
    }

    #[test]
    fn roots_from_minijinja() {
        assert_eq!(
            interpolation_roots("{{ title }} {{ report.content }}"),
            vec!["title".to_string(), "report".to_string()]
        );
    }

    #[test]
    fn content_stitch_render() {
        let mut md = indexmap::IndexMap::new();
        md.insert("content".into(), Value::String("Pokémon".into()));
        let mut scope = BTreeMap::new();
        scope.insert("type_md".into(), Value::Object(md));
        let out = render_program_string("# {{ type_md.content }}", &scope).unwrap();
        assert_eq!(out, "# Pokémon");
    }
}
