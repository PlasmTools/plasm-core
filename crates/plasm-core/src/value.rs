use serde::de::IntoDeserializer;
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

/// Reserved object key for decoded attachment / binary metadata on the wire (see `FieldType::Blob`).
pub const PLASM_ATTACHMENT_KEY: &str = "__plasm_attachment";

/// Typed reference to another program node's output or to a `for_each` row binding (`_`),
/// lowered to plan `__plasm_hole` JSON. **Not** used on ordinary HTTP surface lines — only when
/// the parser is invoked with in-scope program node ids (see [`crate::expr_parser::parse_with_cgs_layers_program`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlasmInputRef {
    /// Materialized program node / binding (alias defaults to `node`).
    NodeInput { node: String, path: Vec<String> },
    /// Row cursor in `source => …` templates (`binding` is typically `"_"`).
    RowBinding { binding: String, path: Vec<String> },
}

impl PlasmInputRef {
    #[must_use]
    pub fn node_output(node: impl Into<String>, path: Vec<String>) -> Self {
        Self::NodeInput {
            node: node.into(),
            path,
        }
    }

    #[must_use]
    pub fn row_binding(binding: impl Into<String>, path: Vec<String>) -> Self {
        Self::RowBinding {
            binding: binding.into(),
            path,
        }
    }
}

impl Serialize for PlasmInputRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeMap;
        let hole = match self {
            PlasmInputRef::NodeInput { node, path } => serde_json::json!({
                "kind": "node_input",
                "node": node,
                "alias": node,
                "path": path,
            }),
            PlasmInputRef::RowBinding { binding, path } => serde_json::json!({
                "kind": "binding",
                "binding": binding,
                "path": path,
            }),
        };
        let mut m = serializer.serialize_map(Some(1))?;
        m.serialize_entry("__plasm_hole", &hole)?;
        m.end()
    }
}

impl<'de> Deserialize<'de> for PlasmInputRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = serde_json::Value::deserialize(deserializer)?;
        let obj = v
            .as_object()
            .ok_or_else(|| de::Error::custom("PlasmInputRef expects a JSON object"))?;
        let inner = obj
            .get("__plasm_hole")
            .ok_or_else(|| de::Error::custom("PlasmInputRef expects __plasm_hole key"))?;
        let kind = inner
            .get("kind")
            .and_then(|k| k.as_str())
            .ok_or_else(|| de::Error::custom("PlasmInputRef hole missing kind"))?;
        match kind {
            "node_input" => {
                let node = inner
                    .get("node")
                    .and_then(|n| n.as_str())
                    .ok_or_else(|| de::Error::custom("node_input hole missing node"))?
                    .to_string();
                let path: Vec<String> = inner
                    .get("path")
                    .and_then(|p| p.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(PlasmInputRef::NodeInput { node, path })
            }
            "binding" => {
                let binding = inner
                    .get("binding")
                    .and_then(|b| b.as_str())
                    .ok_or_else(|| de::Error::custom("binding hole missing binding"))?
                    .to_string();
                let path: Vec<String> = inner
                    .get("path")
                    .and_then(|p| p.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                Ok(PlasmInputRef::RowBinding { binding, path })
            }
            other => Err(de::Error::custom(format!(
                "unknown PlasmInputRef hole kind `{other}`"
            ))),
        }
    }
}

/// Plasm's universal value type supporting JSON-like data plus typed extensions.
///
/// `Integer` and `Float` are kept distinct so that integer field values (e.g. from
/// `FieldType::Integer` schema fields) are serialised as `1` not `1.0` when used
/// as HTTP query parameters — APIs frequently reject `?level=1.0`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    /// Program / template compile-time reference (see [`PlasmInputRef`]).
    PlasmInputRef(PlasmInputRef),
    Null,
    Bool(bool),
    /// Whole-number integer (maps to `FieldType::Integer` and JSON integer literals).
    Integer(i64),
    /// Floating-point number (maps to `FieldType::Number` and JSON fractional literals).
    Float(f64),
    String(String),
    /// Unquoted phrase token from program predicate / invoke RHS (lowered to [`Value::String`] after validation).
    PhraseIdent(String),
    Array(Vec<Value>),
    /// Union variant constructor from teaching table `v101{field=$,…}`; HTTP lowering injects `wire` discriminator.
    UnionCtor {
        #[serde(rename = "__plasm_union_ctor")]
        ctor_label: String,
        #[serde(flatten)]
        ctor_fields: indexmap::IndexMap<String, Value>,
    },
    Object(indexmap::IndexMap<String, Value>),
}

/// Budget for [`Value::format_for_table_cell`] (MCP/HTTP table cells, REPL summaries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueTableCellBudget {
    /// Hard cap on the final UTF-8 byte length (suffix `…` when truncated).
    pub max_total_len: usize,
    /// Recursion depth for nested objects/arrays; beyond this, values summarize as `{n fields}` / `[n items]`.
    pub max_depth: u8,
    pub max_object_entries: usize,
    pub max_array_elements: usize,
}

impl Default for ValueTableCellBudget {
    fn default() -> Self {
        Self {
            max_total_len: 160,
            max_depth: 4,
            max_object_entries: 8,
            max_array_elements: 6,
        }
    }
}

/// Parse `s` as JSON and return a Plasm [`Value`] when the top-level form is an object or array.
///
/// Used for [`FieldType::Json`] slots filled from quoted literals or heredocs whose body is JSON
/// text. Scalar JSON (`"hi"`, `42`, `true`, `null`) returns [`None`] so arbitrary short strings
/// are not silently reinterpreted as JSON payloads.
#[must_use]
pub fn parse_json_subtree_str(s: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(s.trim()).ok()?;
    match &v {
        Value::Object(_) | Value::Array(_) | Value::UnionCtor { .. } => Some(v),
        _ => None,
    }
}

impl Value {
    /// Parse this value as a normalized [`crate::entity_ref_value::EntityRefPayload`] when it is
    /// shaped as an `entity_ref` constructor (atomic or compound tree).
    #[inline]
    pub fn try_as_entity_ref_payload(
        &self,
    ) -> Result<
        crate::entity_ref_value::EntityRefPayload,
        crate::entity_ref_value::EntityRefValueError,
    > {
        crate::entity_ref_value::EntityRefPayload::try_from_value(self)
    }

    /// True when this is the bare string `$` — a teaching prompt fill-in, not a real API value.
    #[inline]
    pub fn is_domain_example_placeholder(&self) -> bool {
        matches!(self, Value::String(s) if s == "$")
    }

    /// Recursively lower [`Value::PhraseIdent`] leaves to [`Value::String`].
    pub fn normalize_phrase_idents_in_tree(&mut self) {
        match self {
            Value::PhraseIdent(s) => *self = Value::String(s.clone()),
            Value::Array(items) => {
                for item in items {
                    item.normalize_phrase_idents_in_tree();
                }
            }
            Value::Object(map) => {
                for v in map.values_mut() {
                    v.normalize_phrase_idents_in_tree();
                }
            }
            Value::UnionCtor { ctor_fields, .. } => {
                for v in ctor_fields.values_mut() {
                    v.normalize_phrase_idents_in_tree();
                }
            }
            Value::PlasmInputRef(_)
            | Value::Null
            | Value::Bool(_)
            | Value::Integer(_)
            | Value::Float(_)
            | Value::String(_) => {}
        }
    }

    /// String payload for [`Value::String`] and pre-lowering [`Value::PhraseIdent`].
    #[inline]
    pub fn as_string_or_phrase(&self) -> Option<&str> {
        match self {
            Value::String(s) | Value::PhraseIdent(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// True when this value carries a string-like payload ([`Value::String`] or pre-lower [`Value::PhraseIdent`]).
    #[inline]
    #[must_use]
    pub fn is_string_like(&self) -> bool {
        self.as_string_or_phrase().is_some()
    }

    /// True if this value or any nested object/array contains the teaching table `$` placeholder.
    pub fn contains_domain_placeholder_deep(&self) -> bool {
        match self {
            Value::String(s) if s == "$" => true,
            Value::String(_) | Value::PhraseIdent(_) => false,
            Value::UnionCtor { ctor_fields, .. } => ctor_fields
                .values()
                .any(Self::contains_domain_placeholder_deep),
            Value::Object(m) => m.values().any(Self::contains_domain_placeholder_deep),
            Value::Array(a) => a.iter().any(Self::contains_domain_placeholder_deep),
            Value::PlasmInputRef(_)
            | Value::Null
            | Value::Bool(_)
            | Value::Integer(_)
            | Value::Float(_) => false,
        }
    }

    /// Get the type name of this value as a string.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::PlasmInputRef(_) => "plasm_input_ref",
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Integer(_) => "integer",
            Value::Float(_) => "float",
            Value::String(_) | Value::PhraseIdent(_) => "string",
            Value::Array(_) => "array",
            Value::UnionCtor { .. } => "union_ctor",
            Value::Object(_) => "object",
        }
    }

    /// Check if this value is compatible with the given field type.
    pub fn is_compatible_with_field_type(&self, field_type: &FieldType) -> bool {
        match (self, field_type) {
            // Compile-time holes defer to plan / runtime materialization.
            (Value::PlasmInputRef(_), _) => true,
            (Value::Null, _) => true,
            (Value::Bool(_), FieldType::Boolean) => true,
            // Integer is compatible with both Integer and Number fields
            (Value::Integer(_), FieldType::Integer | FieldType::Number) => true,
            // Float is compatible with Number fields (and Integer as a relaxed fallback)
            (Value::Float(_), FieldType::Number | FieldType::Integer) => true,
            (
                Value::String(_) | Value::PhraseIdent(_),
                FieldType::String
                | FieldType::Blob
                | FieldType::Uuid
                | FieldType::Select
                | FieldType::Date,
            ) => true,
            // APIs and LLMs often emit numeric literals for string ids / UUID fragments.
            (
                Value::Integer(_) | Value::Float(_),
                FieldType::String | FieldType::Blob | FieldType::Uuid,
            ) => true,
            // Normalized Date values are string (RFC3339 / date) or integer (Unix ms/s) per
            // [`ValueWireFormat::Temporal`] / [`TemporalWireFormat`].
            (Value::Integer(_) | Value::Float(_), FieldType::Date) => true,
            (Value::String(_) | Value::PhraseIdent(_), FieldType::EntityRef { .. }) => true,
            (v, FieldType::Blob) if v.is_plasm_attachment_object() => true,
            // Numeric IDs may arrive as integers for entity refs
            (Value::Integer(_) | Value::Float(_), FieldType::EntityRef { .. }) => true,
            // Compound `entity_ref` scope / predicate values normalize to a structured object
            // (CGS `key_vars` keys) before splat and HTTP binding — see [`crate::entity_ref_value`].
            (Value::Object(_), FieldType::EntityRef { .. }) => {
                crate::entity_ref_value::EntityRefPayload::value_is_legal_shape(self)
            }
            (Value::Array(_), FieldType::Array | FieldType::MultiSelect) => true,
            (Value::Object(_) | Value::Array(_), FieldType::Json) => true,
            (Value::String(s) | Value::PhraseIdent(s), FieldType::Json) => {
                parse_json_subtree_str(s).is_some()
            }
            _ => false,
        }
    }

    /// True when this value is a JSON object carrying [`PLASM_ATTACHMENT_KEY`] metadata (uri, mime, …).
    pub fn is_plasm_attachment_object(&self) -> bool {
        let Some(obj) = self.as_object() else {
            return false;
        };
        let Some(Value::Object(inner)) = obj.get(PLASM_ATTACHMENT_KEY) else {
            return false;
        };
        inner
            .get("uri")
            .is_some_and(|u| matches!(u, Value::String(s) if !s.is_empty()))
            || inner
                .get("bytes_base64")
                .is_some_and(|b| matches!(b, Value::String(s) if !s.is_empty()))
    }

    /// Convert to f64 (covers both Integer and Float variants).
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Integer(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Convert to i64 if this is an integer value.
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(i) => Some(*i),
            Value::Float(f) if f.fract() == 0.0 => Some(*f as i64),
            _ => None,
        }
    }

    /// Convert to a boolean if possible.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Convert to a string if possible.
    pub fn as_str(&self) -> Option<&str> {
        self.as_string_or_phrase()
    }

    /// Convert to an array if possible.
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(arr) => Some(arr),
            _ => None,
        }
    }

    /// Convert to an object if possible.
    pub fn as_object(&self) -> Option<&indexmap::IndexMap<String, Value>> {
        match self {
            Value::Object(obj) => Some(obj),
            _ => None,
        }
    }

    /// Check if this value contains another value (for 'contains' operator).
    pub fn contains(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Array(arr), val) => arr.contains(val),
            (Value::String(s), Value::String(sub) | Value::PhraseIdent(sub)) => s.contains(sub),
            (Value::PhraseIdent(s), Value::String(sub) | Value::PhraseIdent(sub)) => s.contains(sub),
            (Value::Object(obj), Value::String(key)) => obj.contains_key(key),
            _ => false,
        }
    }

    /// Bounded, human-readable string for table cells (Markdown/terminal).
    ///
    /// Recursively formats objects as `key=value` pairs and arrays as `[a, b, …]`, with depth and
    /// entry limits, then clamps the result to [`ValueTableCellBudget::max_total_len`].
    pub fn format_for_table_cell(&self, budget: &ValueTableCellBudget) -> String {
        let s = Self::format_table_cell_inner(self, budget, 0);
        crate::utf8_trunc::truncate_utf8_owned_with_ellipsis(s, budget.max_total_len)
    }

    fn format_table_cell_inner(v: &Value, budget: &ValueTableCellBudget, depth: u8) -> String {
        match v {
            Value::PlasmInputRef(r) => match r {
                PlasmInputRef::NodeInput { node, path } if path.is_empty() => {
                    format!("@{node}")
                }
                PlasmInputRef::NodeInput { node, path } => {
                    format!("@{node}.{}", path.join("."))
                }
                PlasmInputRef::RowBinding { binding, path } if path.is_empty() => {
                    format!("@{binding}")
                }
                PlasmInputRef::RowBinding { binding, path } => {
                    format!("@{binding}.{}", path.join("."))
                }
            },
            Value::Null => "null".to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Float(f) => {
                if f.is_nan() {
                    "nan".to_string()
                } else if f.is_infinite() {
                    if f.is_sign_positive() {
                        "inf".to_string()
                    } else {
                        "-inf".to_string()
                    }
                } else {
                    format!("{f}")
                }
            }
            Value::String(s) | Value::PhraseIdent(s) => {
                crate::utf8_trunc::truncate_utf8_bytes_with_ellipsis(
                    s.as_str(),
                    budget.max_total_len,
                )
            }
            Value::Array(a) => {
                if depth >= budget.max_depth {
                    return format!("[{} items]", a.len());
                }
                if a.is_empty() {
                    return "[]".to_string();
                }
                let take = budget.max_array_elements.min(a.len());
                let mut parts = Vec::with_capacity(take);
                for item in a.iter().take(take) {
                    parts.push(Self::format_table_cell_inner(item, budget, depth + 1));
                }
                let mut out = String::from("[");
                out.push_str(&parts.join(", "));
                append_table_cell_overflow_suffix(&mut out, take, a.len());
                out.push(']');
                out
            }
            Value::UnionCtor {
                ctor_label,
                ctor_fields,
            } => {
                if depth >= budget.max_depth {
                    return format!("{ctor_label}{{…}}");
                }
                let inner = Self::format_table_cell_inner(
                    &Value::Object(ctor_fields.clone()),
                    budget,
                    depth + 1,
                );
                format!("{ctor_label}{{{inner}}}")
            }
            Value::Object(o) => {
                if depth >= budget.max_depth {
                    return format!("{{{} fields}}", o.len());
                }
                if o.is_empty() {
                    return "{}".to_string();
                }
                let take = budget.max_object_entries.min(o.len());
                let mut parts = Vec::with_capacity(take);
                for (k, val) in o.iter().take(take) {
                    let key = format_table_cell_key(k);
                    let val_s = Self::format_table_cell_inner(val, budget, depth + 1);
                    parts.push(format!("{key}={val_s}"));
                }
                let mut out = parts.join(", ");
                append_table_cell_overflow_suffix(&mut out, take, o.len());
                out
            }
        }
    }
}

fn format_table_cell_key(k: &str) -> String {
    if k.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        k.to_string()
    } else {
        serde_json::to_string(k).unwrap_or_else(|_| k.to_string())
    }
}

fn append_table_cell_overflow_suffix(out: &mut String, shown: usize, total: usize) {
    if shown < total {
        out.push_str(&format!(", … (+{} more)", total - shown));
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Self {
        Value::Integer(n)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Value::Integer(n as i64)
    }
}

impl From<usize> for Value {
    fn from(n: usize) -> Self {
        Value::Integer(n as i64)
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Float(n)
    }
}

impl From<f32> for Value {
    fn from(n: f32) -> Self {
        Value::Float(n as f64)
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_string())
    }
}

impl<T: Into<Value>> From<Vec<T>> for Value {
    fn from(vec: Vec<T>) -> Self {
        Value::Array(vec.into_iter().map(|v| v.into()).collect())
    }
}

/// Target wire shape for [`FieldType::Date`] on **input** (path expressions / predicates).
///
/// Normalization applies only there — not when rendering decoded API data for display.
/// Forgiving parse uses [`chrono_english::parse_date_string`](https://docs.rs/chrono-english), then
/// deterministic encoding. Prefer [`ValueWireFormat`] on [`FieldSchema`](crate::schema::FieldSchema)
/// for the full extension point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalWireFormat {
    /// RFC 3339 / ISO-8601 datetime string (UTC `Z` or offset), e.g. `2024-01-15T12:00:00Z`.
    Rfc3339,
    /// Unix time in **milliseconds** as JSON integer (`i64`).
    UnixMs,
    /// Unix time in **seconds** as JSON integer (`i64`).
    UnixSec,
    /// Calendar date only `YYYY-MM-DD` (no timezone component in the wire string).
    Iso8601Date,
}

/// Narrowing of on-wire encoding for a field beyond its scalar [`FieldType`].
///
/// Used when **coercing user/agent input** (path expressions, predicates) to the API’s expected
/// wire shape — not for reformatting values shown for **display** after decoding.
///
/// This is the extension point for **deterministic** normalisation: today time is the main case
/// ([`TemporalWireFormat`]); future variants can cover UUID layout, fixed-scale decimals, etc.
///
/// **YAML / JSON:** A scalar such as `rfc3339` deserialises as [`ValueWireFormat::Temporal`].
/// An explicit map `{ temporal: rfc3339 }` is also accepted (stable once other categories exist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueWireFormat {
    /// Date/time on the wire (see [`TemporalWireFormat`]).
    Temporal(TemporalWireFormat),
}

impl Serialize for ValueWireFormat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ValueWireFormat::Temporal(t) => t.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ValueWireFormat {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct V;

        impl<'de> Visitor<'de> for V {
            type Value = ValueWireFormat;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("value_format string or map with a `temporal` key")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<ValueWireFormat, E> {
                TemporalWireFormat::deserialize(v.into_deserializer())
                    .map(ValueWireFormat::Temporal)
            }

            fn visit_string<E: de::Error>(self, v: String) -> Result<ValueWireFormat, E> {
                self.visit_str(&v)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ValueWireFormat, A::Error> {
                let mut temporal: Option<TemporalWireFormat> = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "temporal" => {
                            if temporal.is_some() {
                                return Err(de::Error::duplicate_field("temporal"));
                            }
                            temporal = Some(map.next_value()?);
                        }
                        other => {
                            return Err(de::Error::unknown_field(other, &["temporal"]));
                        }
                    }
                }
                let t = temporal.ok_or_else(|| {
                    de::Error::custom("value_format map requires a `temporal` field")
                })?;
                Ok(ValueWireFormat::Temporal(t))
            }
        }

        deserializer.deserialize_any(V)
    }
}

/// Field types supported by the Plasm schema system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    Boolean,
    /// Floating-point number (`f64` precision).
    Number,
    /// Whole-number integer (`i64` precision). Serialises as integer in HTTP params.
    Integer,
    /// Canonical UUID string (wire format matches `string`; distinguishes ids in the domain model).
    Uuid,
    /// Opaque binary or large payload field (base64 text, attachment object, etc.).
    /// Prefer `field_type: blob` over `string` + `string_semantics: blob`.
    Blob,
    String,
    Select,      // Single-select with allowed values
    MultiSelect, // Multi-select with allowed values
    Date,
    Array,
    /// Arbitrary JSON object/array subtree from the wire (not a scalar).
    Json,
    /// Foreign key: stores an ID referencing another entity.
    EntityRef {
        target: crate::identity::EntityName,
    },
}

impl FieldType {
    /// Get compatible comparison operators for this field type.
    pub fn compatible_operators(&self) -> &[CompOp] {
        match self {
            FieldType::Boolean => &[CompOp::Eq, CompOp::Neq, CompOp::Exists],
            FieldType::Number | FieldType::Integer => &[
                CompOp::Eq,
                CompOp::Neq,
                CompOp::Gt,
                CompOp::Lt,
                CompOp::Gte,
                CompOp::Lte,
                CompOp::Exists,
            ],
            FieldType::String | FieldType::Blob | FieldType::Uuid | FieldType::Date => {
                &[CompOp::Eq, CompOp::Neq, CompOp::Contains, CompOp::Exists]
            }
            FieldType::Select => &[CompOp::Eq, CompOp::Neq, CompOp::In, CompOp::Exists],
            FieldType::MultiSelect | FieldType::Array => {
                &[CompOp::Contains, CompOp::In, CompOp::Exists]
            }
            FieldType::Json => &[CompOp::Contains, CompOp::Exists],
            FieldType::EntityRef { .. } => &[CompOp::Eq, CompOp::Neq, CompOp::Exists],
        }
    }
}

/// Comparison operators supported in predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompOp {
    #[serde(rename = "=")]
    Eq,
    #[serde(rename = "!=")]
    Neq,
    #[serde(rename = ">")]
    Gt,
    #[serde(rename = "<")]
    Lt,
    #[serde(rename = ">=")]
    Gte,
    #[serde(rename = "<=")]
    Lte,
    #[serde(rename = "in")]
    In,
    #[serde(rename = "contains")]
    Contains,
    #[serde(rename = "exists")]
    Exists,
}

#[cfg(test)]
mod json_subtree_parse_tests {
    use super::*;

    #[test]
    fn parse_object_and_array() {
        let o = parse_json_subtree_str(r#"{"a":1}"#).unwrap();
        assert!(matches!(o, Value::Object(_)));
        let a = parse_json_subtree_str(r#"[1,2]"#).unwrap();
        assert!(matches!(a, Value::Array(_)));
    }

    #[test]
    fn rejects_scalar_json_and_invalid() {
        assert!(parse_json_subtree_str(r#""hi""#).is_none());
        assert!(parse_json_subtree_str("not json").is_none());
    }

    #[test]
    fn string_json_compatible_with_json_field() {
        let v = Value::String(r#"{"x":true}"#.to_string());
        assert!(v.is_compatible_with_field_type(&FieldType::Json));
        let bad = Value::String("plain".to_string());
        assert!(!bad.is_compatible_with_field_type(&FieldType::Json));
    }
}

#[cfg(test)]
mod format_for_table_cell_tests {
    use super::*;
    use indexmap::IndexMap;

    #[test]
    fn flat_object_key_value() {
        let mut m = IndexMap::new();
        m.insert("a".into(), Value::Integer(1));
        m.insert("b".into(), Value::String("x".into()));
        let v = Value::Object(m);
        let s = v.format_for_table_cell(&ValueTableCellBudget::default());
        assert!(s.contains("a=1"));
        assert!(s.contains("b=x"));
    }

    #[test]
    fn nested_object_summarizes_at_depth() {
        let inner = Value::Object({
            let mut m = IndexMap::new();
            m.insert("k".into(), Value::Bool(true));
            m
        });
        let mut outer = IndexMap::new();
        outer.insert("inner".into(), inner);
        let v = Value::Object(outer);
        let budget = ValueTableCellBudget {
            max_total_len: 200,
            max_depth: 1,
            max_object_entries: 8,
            max_array_elements: 6,
        };
        let s = v.format_for_table_cell(&budget);
        assert!(s.contains("inner="));
        assert!(s.contains("{1 fields}"));
    }

    #[test]
    fn array_shows_elements() {
        let v = Value::Array(vec![Value::Integer(1), Value::Null]);
        let s = v.format_for_table_cell(&ValueTableCellBudget::default());
        assert_eq!(s, "[1, null]");
    }

    #[test]
    fn array_truncates_extra_count() {
        let v = Value::Array(vec![Value::Integer(0); 10]);
        let s = v.format_for_table_cell(&ValueTableCellBudget::default());
        assert!(s.starts_with('['));
        assert!(s.contains("… (+4 more)"));
        assert!(s.ends_with(']'));
    }

    #[test]
    fn clamps_total_utf8_length() {
        let long = "α".repeat(200);
        let v = Value::String(long);
        let s = v.format_for_table_cell(&ValueTableCellBudget::default());
        assert!(s.len() <= 160);
        assert!(s.ends_with('…'));
    }

    #[test]
    fn quotes_non_simple_object_keys() {
        let mut m = IndexMap::new();
        m.insert("a b".into(), Value::Integer(1));
        let v = Value::Object(m);
        let s = v.format_for_table_cell(&ValueTableCellBudget::default());
        assert!(s.contains("\"a b\"=1"));
    }
}

#[cfg(test)]
mod value_wire_format_tests {
    use super::*;

    #[test]
    fn deserializes_temporal_scalar_string() {
        let v: ValueWireFormat = serde_json::from_str("\"rfc3339\"").unwrap();
        assert_eq!(v, ValueWireFormat::Temporal(TemporalWireFormat::Rfc3339));
    }

    #[test]
    fn deserializes_explicit_temporal_map() {
        let v: ValueWireFormat =
            serde_json::from_value(serde_json::json!({ "temporal": "unix_ms" })).unwrap();
        assert_eq!(v, ValueWireFormat::Temporal(TemporalWireFormat::UnixMs));
    }

    #[test]
    fn serializes_as_temporal_scalar() {
        let j = serde_json::to_string(&ValueWireFormat::Temporal(TemporalWireFormat::Iso8601Date))
            .unwrap();
        assert_eq!(j, "\"iso8601_date\"");
    }
}
