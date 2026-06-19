use crate::DecodeError;
use indexmap::IndexMap;
use plasm_core::{Cardinality, FieldDeriveRule, Ref, Value};
use serde::{Deserialize, Serialize};

/// Entity decoder - specifies how to extract entities from API responses
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityDecoder {
    pub entity: String,
    pub source: PathExpr,
    pub fields: Vec<FieldDecoder>,
    pub relations: Vec<RelationDecoder>,
    /// When set, this JSON object key is tried first as the entity id (CGS `id_field`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_field: Option<String>,
    /// When set, identity is read from this path (object keys) instead of top-level id keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_path: Option<PathExpr>,
    /// When set, use this id for [`DecodedEntity::reference`] (GET path id) when the body has no row id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_identity_override: Option<String>,
    /// CGS `key_vars` — when length ≥ 2, [`DecodedEntity::reference`] is a compound [`Ref`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_vars: Vec<String>,
    /// Scope / request bindings (CML env or GET ref) merged when a key part is missing from the row.
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub identity_ambient: IndexMap<String, String>,
    /// Parent row field paths/derives for compound child refs (e.g. Issue `owner`/`repo` → Label).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_identity_field_hints: Vec<ParentIdentityFieldHint>,
}

/// Parent scalar slot used when decoding nested relation rows (compound child `key_vars`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParentIdentityFieldHint {
    pub slot: String,
    pub from: PathExpr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derive: Option<FieldDeriveRule>,
}

/// Field decoder - specifies how to extract a single field
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldDecoder {
    pub field: String,
    pub from: PathExpr,
    pub transform: Option<Transform>,
    /// Post-extraction derivation from wire JSON (before [`Transform`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derive: Option<FieldDeriveRule>,
}

/// Relation decoder - specifies how to extract related entities
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationDecoder {
    pub relation: String,
    pub decoder: EntityDecoder,
    pub cardinality: Cardinality,
}

/// Path expression for navigating JSON structures
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PathExpr {
    pub segments: Vec<PathSegment>,
}

/// A segment in a path expression
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PathSegment {
    /// Object key access
    #[serde(rename = "key")]
    Key { name: String },
    /// Array index access
    #[serde(rename = "index")]
    Index { index: usize },
    /// Wildcard - map over array elements
    #[serde(rename = "wildcard")]
    Wildcard,
}

/// Value transformation functions
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Transform {
    #[serde(rename = "to_number")]
    ToNumber,
    #[serde(rename = "to_string")]
    ToString,
    #[serde(rename = "to_bool")]
    ToBool,
    #[serde(rename = "map_enum")]
    MapEnum { mapping: IndexMap<String, String> },
    #[serde(rename = "identity")]
    Identity,
}

/// Whether a declared relation was present in the wire payload or omitted (not projected).
///
/// Omitted relations must not overwrite previously cached refs during cache merge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DecodedRelation {
    /// Response did not include this relation's path (or it was null before the collection).
    Unspecified,
    /// Relation was projected; empty means an authoritative empty edge set.
    Specified(Vec<Ref>),
}

/// A decoded entity instance
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecodedEntity {
    pub reference: Ref,
    pub fields: IndexMap<String, Value>,
    pub relations: IndexMap<String, DecodedRelation>,
    /// Fully decoded `from_parent_get` relation targets (also inserted into graph cache at parent GET).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedded_entities: Vec<DecodedEntity>,
}

impl PathExpr {
    /// Create a new path expression
    pub fn new(segments: Vec<PathSegment>) -> Self {
        Self { segments }
    }

    /// Create a path from a slice notation like ["results", "*", "properties", "Name"]
    pub fn from_slice(parts: &[&str]) -> Self {
        let segments = parts
            .iter()
            .map(|&part| {
                if part == "*" {
                    PathSegment::Wildcard
                } else if let Ok(index) = part.parse::<usize>() {
                    PathSegment::Index { index }
                } else {
                    PathSegment::Key {
                        name: part.to_string(),
                    }
                }
            })
            .collect();

        Self { segments }
    }

    /// Create an empty path
    pub fn empty() -> Self {
        Self {
            segments: Vec::new(),
        }
    }

    /// Add a key segment
    pub fn key(mut self, name: impl Into<String>) -> Self {
        self.segments.push(PathSegment::Key { name: name.into() });
        self
    }

    /// Add an index segment
    pub fn index(mut self, index: usize) -> Self {
        self.segments.push(PathSegment::Index { index });
        self
    }

    /// Add a wildcard segment
    pub fn wildcard(mut self) -> Self {
        self.segments.push(PathSegment::Wildcard);
        self
    }
}

impl PathSegment {
    /// Create a key segment
    pub fn key(name: impl Into<String>) -> Self {
        PathSegment::Key { name: name.into() }
    }

    /// Create an index segment
    pub fn index(index: usize) -> Self {
        PathSegment::Index { index }
    }

    /// Create a wildcard segment
    pub fn wildcard() -> Self {
        PathSegment::Wildcard
    }
}

impl FieldDecoder {
    /// Create a new field decoder
    pub fn new(field: impl Into<String>, from: PathExpr) -> Self {
        Self {
            field: field.into(),
            from,
            transform: None,
            derive: None,
        }
    }

    /// Add a transform
    pub fn with_transform(mut self, transform: Transform) -> Self {
        self.transform = Some(transform);
        self
    }

    pub fn with_derive(mut self, derive: FieldDeriveRule) -> Self {
        self.derive = Some(derive);
        self
    }
}

impl EntityDecoder {
    /// Create a new entity decoder
    pub fn new(entity: impl Into<String>, source: PathExpr) -> Self {
        Self {
            entity: entity.into(),
            source,
            fields: Vec::new(),
            relations: Vec::new(),
            id_field: None,
            id_path: None,
            request_identity_override: None,
            key_vars: Vec::new(),
            identity_ambient: IndexMap::new(),
            parent_identity_field_hints: Vec::new(),
        }
    }

    pub fn with_parent_identity_field_hints(mut self, hints: Vec<ParentIdentityFieldHint>) -> Self {
        self.parent_identity_field_hints = hints;
        self
    }

    /// Compound-key parts from CGS `key_vars`, in declaration order.
    pub fn with_key_vars(mut self, vars: Vec<String>) -> Self {
        self.key_vars = vars;
        self
    }

    /// Ambient identity slots (query scope / GET ref) merged after row fields.
    pub fn with_identity_ambient(mut self, ambient: IndexMap<String, String>) -> Self {
        self.identity_ambient = ambient;
        self
    }

    /// Use the HTTP GET primary id when JSON has no stable [`id_field`] (subresource reads).
    pub fn with_request_identity_override(mut self, id: impl Into<String>) -> Self {
        self.request_identity_override = Some(id.into());
        self
    }

    /// Prefer this JSON key when resolving [`DecodedEntity::reference`] ids.
    pub fn with_id_field(mut self, id_field: impl Into<String>) -> Self {
        self.id_field = Some(id_field.into());
        self
    }

    /// Resolve identity from a nested path (e.g. `location_area` → `url`).
    pub fn with_id_path(mut self, path: PathExpr) -> Self {
        self.id_path = Some(path);
        self
    }

    /// Add field decoders
    pub fn with_fields(mut self, fields: Vec<FieldDecoder>) -> Self {
        self.fields = fields;
        self
    }

    /// Add relation decoders
    pub fn with_relations(mut self, relations: Vec<RelationDecoder>) -> Self {
        self.relations = relations;
        self
    }
}

/// Extract values from JSON using a path expression
pub fn extract_path(
    path: &PathExpr,
    json: &serde_json::Value,
) -> Result<Vec<serde_json::Value>, DecodeError> {
    let mut current = vec![json.clone()];

    for segment in &path.segments {
        let mut next = Vec::new();

        for value in current {
            match segment {
                PathSegment::Key { name } => {
                    if value.is_null() {
                        // Null is a legitimate optional value (e.g. evolves_from_species: null).
                        // Treat as missing — produce no output for this path branch.
                    } else if let Some(obj) = value.as_object() {
                        if let Some(field_value) = obj.get(name) {
                            next.push(field_value.clone());
                        }
                        // If key doesn't exist, skip silently (optional fields)
                    } else {
                        return Err(DecodeError::TypeMismatch {
                            path: format_path_up_to(&path.segments, segment),
                            expected: "object".to_string(),
                            found: value_type_name(&value).to_string(),
                        });
                    }
                }

                PathSegment::Index { index } => {
                    if let Some(arr) = value.as_array() {
                        if let Some(element) = arr.get(*index) {
                            next.push(element.clone());
                        } else {
                            return Err(DecodeError::PathNotFound {
                                path: format_path_up_to(&path.segments, segment),
                            });
                        }
                    } else {
                        return Err(DecodeError::TypeMismatch {
                            path: format_path_up_to(&path.segments, segment),
                            expected: "array".to_string(),
                            found: value_type_name(&value).to_string(),
                        });
                    }
                }

                PathSegment::Wildcard => {
                    if let Some(arr) = value.as_array() {
                        next.extend(arr.iter().cloned());
                    } else {
                        return Err(DecodeError::TypeMismatch {
                            path: format_path_up_to(&path.segments, segment),
                            expected: "array".to_string(),
                            found: value_type_name(&value).to_string(),
                        });
                    }
                }
            }
        }

        current = next;
    }

    Ok(current)
}

/// Split `s` on `:` and return `"<segment>:<next>"` when `segment` appears as a whole segment with a
/// non-empty following segment (used for snapshot block refs such as `snapshot:id:mt1:hash:b0`).
fn colon_segment_pair_extract(s: &str, segment: &str) -> Option<String> {
    let parts: Vec<&str> = s.split(':').collect();
    for i in 0..parts.len() {
        if parts[i] == segment {
            return parts
                .get(i + 1)
                .copied()
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(|h| format!("{segment}:{h}"));
        }
    }
    None
}

pub(crate) fn apply_field_derive_rule(
    rule: &FieldDeriveRule,
    v: &serde_json::Value,
) -> Result<serde_json::Value, DecodeError> {
    match rule {
        FieldDeriveRule::SegmentsAfterPrefix {
            prefix,
            alternate_prefixes,
            part_index,
        } => {
            let s = v.as_str().ok_or_else(|| DecodeError::InvalidStructure {
                message: "segments_after_prefix derive requires a JSON string value".to_string(),
            })?;
            let mut rest: Option<&str> = s.strip_prefix(prefix.as_str());
            if rest.is_none() {
                for alt in alternate_prefixes {
                    if let Some(r) = s.strip_prefix(alt.as_str()) {
                        rest = Some(r);
                        break;
                    }
                }
            }
            let rest = rest.ok_or_else(|| DecodeError::InvalidStructure {
                message: format!(
                    "segments_after_prefix: value does not start with prefix {prefix:?} or alternates"
                ),
            })?;
            let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
            let seg = parts
                .get(*part_index)
                .ok_or_else(|| DecodeError::InvalidStructure {
                    message: format!(
                    "segments_after_prefix: part_index {part_index} out of range (got {} segments)",
                    parts.len()
                ),
                })?;
            let seg = seg.trim_end_matches(".git");
            Ok(serde_json::Value::String(seg.to_string()))
        }
        FieldDeriveRule::NameValueArrayLookup {
            equals,
            match_key_field,
            value_field,
            case_insensitive,
        } => {
            let arr = v.as_array().ok_or_else(|| DecodeError::InvalidStructure {
                message: "name_value_array_lookup derive requires a JSON array value".to_string(),
            })?;
            for item in arr {
                let Some(obj) = item.as_object() else {
                    continue;
                };
                let Some(mk) = obj.get(match_key_field.as_str()) else {
                    continue;
                };
                let Some(mk_str) = mk.as_str() else {
                    continue;
                };
                let matches = if *case_insensitive {
                    mk_str.eq_ignore_ascii_case(equals.as_str())
                } else {
                    mk_str == equals.as_str()
                };
                if !matches {
                    continue;
                }
                return Ok(obj
                    .get(value_field.as_str())
                    .cloned()
                    .unwrap_or(serde_json::Value::Null));
            }
            Ok(serde_json::Value::Null)
        }
        FieldDeriveRule::ObjectKeyLookup {
            key,
            case_insensitive,
        } => {
            let obj = v.as_object().ok_or_else(|| DecodeError::InvalidStructure {
                message: "object_key_lookup derive requires a JSON object value".to_string(),
            })?;
            if *case_insensitive {
                for (k, val) in obj {
                    if k.eq_ignore_ascii_case(key.as_str()) {
                        return Ok(val.clone());
                    }
                }
                Ok(serde_json::Value::Null)
            } else {
                Ok(obj
                    .get(key.as_str())
                    .cloned()
                    .unwrap_or(serde_json::Value::Null))
            }
        }
        FieldDeriveRule::ColonPairPreferKeysElseBlockRefs {
            prefer_keys,
            blocks_key,
            ref_field,
            segment,
        } => {
            let Some(obj) = v.as_object() else {
                return Ok(serde_json::Value::Null);
            };
            for pk in prefer_keys {
                let Some(serde_json::Value::String(s)) = obj.get(pk.as_str()) else {
                    continue;
                };
                let t = s.trim();
                if t.is_empty() {
                    continue;
                }
                if let Some(out) = colon_segment_pair_extract(t, segment.as_str()) {
                    return Ok(serde_json::Value::String(out));
                }
            }
            let Some(serde_json::Value::Array(blocks)) = obj.get(blocks_key.as_str()) else {
                return Ok(serde_json::Value::Null);
            };
            for item in blocks {
                let Some(bobj) = item.as_object() else {
                    continue;
                };
                let Some(serde_json::Value::String(rs)) = bobj.get(ref_field.as_str()) else {
                    continue;
                };
                let rt = rs.trim();
                if rt.is_empty() {
                    continue;
                }
                if let Some(out) = colon_segment_pair_extract(rt, segment.as_str()) {
                    return Ok(serde_json::Value::String(out));
                }
            }
            Ok(serde_json::Value::Null)
        }
    }
}

/// Apply a transform to a value
pub fn apply_transform(
    transform: &Transform,
    value: &serde_json::Value,
) -> Result<Value, DecodeError> {
    match transform {
        Transform::Identity => Ok(json_to_value(value)),

        Transform::ToString => match value {
            serde_json::Value::String(s) => Ok(Value::String(s.clone())),
            serde_json::Value::Number(n) => Ok(Value::String(n.to_string())),
            serde_json::Value::Bool(b) => Ok(Value::String(b.to_string())),
            _ => Err(DecodeError::TransformFailed {
                transform: "to_string".to_string(),
                value: value.to_string(),
                reason: "Value cannot be converted to string".to_string(),
            }),
        },

        Transform::ToNumber => match value {
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Value::Integer(i))
                } else if let Some(f) = n.as_f64() {
                    Ok(Value::Float(f))
                } else {
                    Err(DecodeError::TransformFailed {
                        transform: "to_number".to_string(),
                        value: value.to_string(),
                        reason: "Number is not a valid f64".to_string(),
                    })
                }
            }
            serde_json::Value::String(s) => {
                if let Ok(i) = s.parse::<i64>() {
                    Ok(Value::Integer(i))
                } else if let Ok(f) = s.parse::<f64>() {
                    Ok(Value::Float(f))
                } else {
                    Err(DecodeError::TransformFailed {
                        transform: "to_number".to_string(),
                        value: value.to_string(),
                        reason: "String cannot be parsed as number".to_string(),
                    })
                }
            }
            _ => Err(DecodeError::TransformFailed {
                transform: "to_number".to_string(),
                value: value.to_string(),
                reason: "Value is not a number or string".to_string(),
            }),
        },

        Transform::ToBool => match value {
            serde_json::Value::Bool(b) => Ok(Value::Bool(*b)),
            serde_json::Value::String(s) => match s.to_lowercase().as_str() {
                "true" | "yes" | "1" => Ok(Value::Bool(true)),
                "false" | "no" | "0" => Ok(Value::Bool(false)),
                _ => Err(DecodeError::TransformFailed {
                    transform: "to_bool".to_string(),
                    value: value.to_string(),
                    reason: "String is not a valid boolean".to_string(),
                }),
            },
            _ => Err(DecodeError::TransformFailed {
                transform: "to_bool".to_string(),
                value: value.to_string(),
                reason: "Value is not a boolean or string".to_string(),
            }),
        },

        Transform::MapEnum { mapping } => {
            if let Some(key_str) = value.as_str() {
                if let Some(mapped) = mapping.get(key_str) {
                    Ok(Value::String(mapped.clone()))
                } else {
                    Ok(Value::String(key_str.to_string())) // Pass through if not in mapping
                }
            } else {
                Err(DecodeError::TransformFailed {
                    transform: "map_enum".to_string(),
                    value: value.to_string(),
                    reason: "Value is not a string".to_string(),
                })
            }
        }
    }
}

/// True when the relation's [`EntityDecoder::source`] path is present enough to treat the decode as authoritative:
/// every key segment exists, values are non-null along the walk, and a terminal wildcard sits on a JSON array.
///
/// If the first key is missing (common for omitted GraphQL selections), returns false so the relation is
/// [`DecodedRelation::Unspecified`] and cache merge will not treat it as an empty edge set.
pub fn relation_decode_path_specified(value: &serde_json::Value, path: &PathExpr) -> bool {
    let mut cur = value;
    for seg in &path.segments {
        match seg {
            PathSegment::Key { name } => {
                let Some(obj) = cur.as_object() else {
                    return false;
                };
                if !obj.contains_key(name) {
                    return false;
                }
                cur = &obj[name];
                if cur.is_null() {
                    return false;
                }
            }
            PathSegment::Index { index } => {
                let Some(arr) = cur.as_array() else {
                    return false;
                };
                let Some(next) = arr.get(*index) else {
                    return false;
                };
                cur = next;
                if cur.is_null() {
                    return false;
                }
            }
            PathSegment::Wildcard => {
                return cur.is_array();
            }
        }
    }
    true
}

/// Convert serde_json::Value to plasm_core::Value
pub(crate) fn json_to_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(arr) => {
            let values = arr.iter().map(json_to_value).collect();
            Value::Array(values)
        }
        serde_json::Value::Object(obj) => {
            let mut map = IndexMap::new();
            for (k, v) in obj {
                map.insert(k.clone(), json_to_value(v));
            }
            Value::Object(map)
        }
    }
}

/// Get the type name of a JSON value
fn value_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Format a path up to a specific segment for error messages
fn format_path_up_to(segments: &[PathSegment], up_to: &PathSegment) -> String {
    let mut path_parts = Vec::new();

    for segment in segments {
        match segment {
            PathSegment::Key { name } => path_parts.push(name.clone()),
            PathSegment::Index { index } => path_parts.push(format!("[{}]", index)),
            PathSegment::Wildcard => path_parts.push("*".to_string()),
        }

        if segment == up_to {
            break;
        }
    }

    path_parts.join(".")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode_entities;
    use serde_json::json;

    #[test]
    fn test_extract_simple_path() {
        let json = json!({
            "data": {
                "name": "test"
            }
        });

        let path = PathExpr::from_slice(&["data", "name"]);
        let result = extract_path(&path, &json).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], json!("test"));
    }

    #[test]
    fn test_extract_wildcard_path() {
        let json = json!({
            "results": [
                {"id": "1", "name": "first"},
                {"id": "2", "name": "second"}
            ]
        });

        let path = PathExpr::from_slice(&["results", "*", "name"]);
        let result = extract_path(&path, &json).unwrap();

        assert_eq!(result.len(), 2);
        assert_eq!(result[0], json!("first"));
        assert_eq!(result[1], json!("second"));
    }

    #[test]
    fn test_extract_array_index() {
        let json = json!({
            "items": ["first", "second", "third"]
        });

        let path = PathExpr::from_slice(&["items", "1"]);
        let result = extract_path(&path, &json).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0], json!("second"));
    }

    #[test]
    fn test_transform_to_string() {
        let value = json!(123);
        let transform = Transform::ToString;
        let result = apply_transform(&transform, &value).unwrap();

        assert_eq!(result, Value::String("123".to_string()));
    }

    #[test]
    fn test_transform_to_number() {
        let value = json!("456.78");
        let transform = Transform::ToNumber;
        let result = apply_transform(&transform, &value).unwrap();

        assert_eq!(result, Value::Float(456.78));
    }

    #[test]
    fn test_transform_map_enum() {
        let mut mapping = IndexMap::new();
        mapping.insert("active".to_string(), "ACTIVE".to_string());
        mapping.insert("inactive".to_string(), "INACTIVE".to_string());

        let transform = Transform::MapEnum { mapping };
        let value = json!("active");
        let result = apply_transform(&transform, &value).unwrap();

        assert_eq!(result, Value::String("ACTIVE".to_string()));
    }

    #[test]
    fn test_decode_simple_entity() {
        let json = json!({
            "results": [
                {
                    "id": "acc-1",
                    "properties": {
                        "Name": {"title": [{"text": {"content": "Acme Corp"}}]},
                        "Revenue": {"number": 1200}
                    }
                }
            ]
        });

        let decoder = EntityDecoder::new("Account", PathExpr::from_slice(&["results", "*"]))
            .with_fields(vec![
                FieldDecoder::new(
                    "name",
                    PathExpr::from_slice(&["properties", "Name", "title", "0", "text", "content"]),
                ),
                FieldDecoder::new(
                    "revenue",
                    PathExpr::from_slice(&["properties", "Revenue", "number"]),
                ),
            ]);

        let entities = decode_entities(&decoder, &json).unwrap();

        assert_eq!(entities.len(), 1);
        let entity = &entities[0];
        assert_eq!(entity.reference.entity_type, "Account");
        assert_eq!(entity.reference.simple_id().unwrap().as_str(), "acc-1");
        assert_eq!(
            entity.fields.get("name"),
            Some(&Value::String("Acme Corp".to_string()))
        );
        assert_eq!(entity.fields.get("revenue"), Some(&Value::Integer(1200)));
    }

    #[test]
    fn decode_compound_ref_from_nested_key_fields() {
        let json = json!({
            "items": [{
                "id": 42,
                "name": "hello",
                "owner": {"login": "octocat"}
            }]
        });
        let decoder = EntityDecoder::new("Repository", PathExpr::from_slice(&["items", "*"]))
            .with_fields(vec![
                FieldDecoder::new("id", PathExpr::from_slice(&["id"])),
                FieldDecoder::new("repo", PathExpr::from_slice(&["name"])),
                FieldDecoder::new("owner", PathExpr::from_slice(&["owner", "login"])),
            ])
            .with_id_field("id")
            .with_key_vars(vec!["owner".into(), "repo".into()]);

        let entities = decode_entities(&decoder, &json).unwrap();
        assert_eq!(entities.len(), 1);
        let parts = entities[0]
            .reference
            .compound_parts()
            .expect("compound ref");
        assert_eq!(parts.get("owner").map(String::as_str), Some("octocat"));
        assert_eq!(parts.get("repo").map(String::as_str), Some("hello"));
    }

    #[test]
    fn decode_segments_after_prefix_derives_owner_repo() {
        let json = json!({
            "items": [{
                "id": 10,
                "number": 1,
                "repository_url": "https://api.github.com/repos/octocat/Hello-World"
            }]
        });
        let decoder =
            EntityDecoder::new("Issue", PathExpr::from_slice(&["items", "*"]))
                .with_fields(vec![
                    FieldDecoder::new("id", PathExpr::from_slice(&["id"])),
                    FieldDecoder::new("number", PathExpr::from_slice(&["number"])),
                    FieldDecoder::new("owner", PathExpr::from_slice(&["repository_url"]))
                        .with_derive(FieldDeriveRule::SegmentsAfterPrefix {
                            prefix: "https://api.github.com/repos/".into(),
                            alternate_prefixes: vec![],
                            part_index: 0,
                        }),
                    FieldDecoder::new("repo", PathExpr::from_slice(&["repository_url"]))
                        .with_derive(FieldDeriveRule::SegmentsAfterPrefix {
                            prefix: "https://api.github.com/repos/".into(),
                            alternate_prefixes: vec![],
                            part_index: 1,
                        }),
                ])
                .with_id_field("id")
                .with_key_vars(vec!["owner".into(), "repo".into(), "number".into()]);

        let entities = decode_entities(&decoder, &json).unwrap();
        let parts = entities[0].reference.compound_parts().unwrap();
        assert_eq!(parts.get("owner").map(String::as_str), Some("octocat"));
        assert_eq!(parts.get("repo").map(String::as_str), Some("Hello-World"));
        assert_eq!(parts.get("number").map(String::as_str), Some("1"));
    }

    #[test]
    fn decode_compound_ref_fills_missing_part_from_identity_ambient() {
        let json = json!({"items": [{"id": "e1", "summary": "Hi"}]});
        let mut ambient = IndexMap::new();
        ambient.insert("calendarId".into(), "cal".into());
        let decoder = EntityDecoder::new("Event", PathExpr::from_slice(&["items", "*"]))
            .with_fields(vec![
                FieldDecoder::new("id", PathExpr::from_slice(&["id"])),
                FieldDecoder::new("summary", PathExpr::from_slice(&["summary"])),
            ])
            .with_id_field("id")
            .with_key_vars(vec!["calendarId".into(), "id".into()])
            .with_identity_ambient(ambient);

        let entities = decode_entities(&decoder, &json).unwrap();
        let parts = entities[0].reference.compound_parts().unwrap();
        assert_eq!(parts.get("calendarId").map(String::as_str), Some("cal"));
        assert_eq!(parts.get("id").map(String::as_str), Some("e1"));
    }

    #[test]
    fn decode_nested_relation_compound_key_from_parent_root() {
        let json = json!({
            "spreadsheetId": "abc123",
            "sheets": [{ "properties": { "sheetId": 0, "title": "Sheet1" } }]
        });

        let sheet_decoder = EntityDecoder::new("Sheet", PathExpr::from_slice(&["sheets", "*"]))
            .with_id_field("sheetId")
            .with_id_path(PathExpr::from_slice(&["properties", "sheetId"]))
            .with_key_vars(vec!["spreadsheetId".into(), "sheetId".into()]);

        let parent_decoder = EntityDecoder::new("Spreadsheet", PathExpr::empty())
            .with_fields(vec![FieldDecoder::new(
                "spreadsheetId",
                PathExpr::from_slice(&["spreadsheetId"]),
            )])
            .with_id_field("spreadsheetId")
            .with_relations(vec![RelationDecoder {
                relation: "sheets".into(),
                decoder: sheet_decoder,
                cardinality: plasm_core::Cardinality::Many,
            }]);

        let entities = decode_entities(&parent_decoder, &json).unwrap();
        assert_eq!(entities.len(), 1);
        let spreadsheet = &entities[0];
        assert_eq!(spreadsheet.reference.entity_type, "Spreadsheet");
        assert_eq!(
            spreadsheet.reference.simple_id().unwrap().as_str(),
            "abc123"
        );

        let sheets_rel = spreadsheet
            .relations
            .get("sheets")
            .expect("sheets relation");
        match sheets_rel {
            DecodedRelation::Specified(refs) => {
                assert_eq!(refs.len(), 1);
                let parts = refs[0].compound_parts().expect("compound Sheet ref");
                assert_eq!(
                    parts.get("spreadsheetId").map(String::as_str),
                    Some("abc123")
                );
                assert_eq!(parts.get("sheetId").map(String::as_str), Some("0"));
            }
            DecodedRelation::Unspecified => panic!("expected Specified"),
        }
    }

    /// Regression: Sheets `spreadsheets.get` returns `spreadsheetId` at the root (public sample layout).
    /// Decoding must not report “No valid ID field” for this shape.
    #[test]
    fn decode_sheets_v4_get_root_matches_spreadsheet_decoder() {
        let json = json!({
            "spreadsheetId": "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms",
            "properties": { "title": "Class Data", "locale": "en_US", "timeZone": "America/New_York" },
            "spreadsheetUrl": "https://docs.google.com/spreadsheets/d/1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms/edit",
            "sheets": [{ "properties": { "sheetId": 0, "title": "Class Data", "index": 0, "sheetType": "GRID" } }]
        });

        let sheet_decoder = EntityDecoder::new("Sheet", PathExpr::from_slice(&["sheets", "*"]))
            .with_id_field("sheetId")
            .with_id_path(PathExpr::from_slice(&["properties", "sheetId"]))
            .with_key_vars(vec!["spreadsheetId".into(), "sheetId".into()]);

        let parent_decoder = EntityDecoder::new("Spreadsheet", PathExpr::empty())
            .with_fields(vec![
                FieldDecoder::new("spreadsheetId", PathExpr::from_slice(&["spreadsheetId"])),
                FieldDecoder::new("title", PathExpr::from_slice(&["properties", "title"])),
                FieldDecoder::new("locale", PathExpr::from_slice(&["properties", "locale"])),
                FieldDecoder::new(
                    "timeZone",
                    PathExpr::from_slice(&["properties", "timeZone"]),
                ),
                FieldDecoder::new("spreadsheetUrl", PathExpr::from_slice(&["spreadsheetUrl"])),
            ])
            .with_id_field("spreadsheetId")
            .with_relations(vec![RelationDecoder {
                relation: "sheets".into(),
                decoder: sheet_decoder,
                cardinality: plasm_core::Cardinality::Many,
            }]);

        let entities = decode_entities(&parent_decoder, &json).unwrap();
        assert_eq!(entities.len(), 1);
        let spreadsheet = &entities[0];
        assert_eq!(spreadsheet.reference.entity_type, "Spreadsheet");
        assert_eq!(
            spreadsheet.reference.simple_id().unwrap().as_str(),
            "1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgvE2upms"
        );
        assert_eq!(
            spreadsheet.fields.get("title"),
            Some(&Value::String("Class Data".into()))
        );
    }

    #[test]
    fn test_decode_scalar_id_rows_from_feed_array() {
        let json = json!([1_i64, 2_i64, 3_i64]);
        let decoder =
            EntityDecoder::new("Item", PathExpr::from_slice(&["results", "*"])).with_id_field("id");

        let wrapped = serde_json::json!({ "results": json });
        let entities = decode_entities(&decoder, &wrapped).unwrap();
        assert_eq!(entities.len(), 3);
        assert_eq!(entities[0].reference.simple_id().unwrap().as_str(), "1");
        assert_eq!(entities[1].reference.simple_id().unwrap().as_str(), "2");
        assert_eq!(entities[0].fields.get("id"), Some(&Value::Integer(1)));
    }

    #[test]
    fn test_decode_id_from_nested_path() {
        let json = json!({
            "results": [
                {
                    "location_area": { "name": "route-3", "url": "https://pokeapi.co/api/v2/location-area/281/" },
                    "version_details": []
                }
            ]
        });
        let id_path = PathExpr::new(vec![
            PathSegment::Key {
                name: "location_area".into(),
            },
            PathSegment::Key { name: "url".into() },
        ]);
        let decoder = EntityDecoder::new("EncounterRow", PathExpr::from_slice(&["results", "*"]))
            .with_id_field("row_id")
            .with_id_path(id_path)
            .with_fields(vec![]);

        let entities = decode_entities(&decoder, &json).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(
            entities[0].reference.simple_id().unwrap().as_str(),
            "https://pokeapi.co/api/v2/location-area/281/"
        );
        assert_eq!(
            entities[0].fields.get("row_id"),
            Some(&Value::String(
                "https://pokeapi.co/api/v2/location-area/281/".into()
            ))
        );
    }

    #[test]
    fn test_missing_path_returns_empty() {
        let json = json!({
            "other": "data"
        });

        let path = PathExpr::from_slice(&["missing"]);
        let result = extract_path(&path, &json).unwrap();

        // Missing key should return empty result
        assert!(result.is_empty());
    }

    #[test]
    fn decode_card_one_relation_url_hyperlink_id() {
        let json = json!({
            "name": "pikachu",
            "evolution_chain": {
                "url": "https://pokeapi.co/api/v2/evolution-chain/10/"
            }
        });
        let chain_decoder =
            EntityDecoder::new("EvolutionChain", PathExpr::from_slice(&["evolution_chain"]))
                .with_id_field("url")
                .with_id_path(PathExpr::from_slice(&["url"]));
        let parent = EntityDecoder::new("PokemonSpecies", PathExpr::empty())
            .with_id_field("name")
            .with_fields(vec![FieldDecoder::new(
                "name",
                PathExpr::from_slice(&["name"]),
            )])
            .with_relations(vec![RelationDecoder {
                relation: "evolution_chain".into(),
                decoder: chain_decoder,
                cardinality: plasm_core::Cardinality::One,
            }]);
        let entities = decode_entities(&parent, &json).unwrap();
        assert_eq!(entities.len(), 1);
        let rel = entities[0]
            .relations
            .get("evolution_chain")
            .expect("evolution_chain relation");
        match rel {
            DecodedRelation::Specified(refs) => {
                assert_eq!(refs.len(), 1);
                assert_eq!(
                    refs[0].simple_id().unwrap().as_str(),
                    "https://pokeapi.co/api/v2/evolution-chain/10/"
                );
            }
            DecodedRelation::Unspecified => panic!("expected Specified evolution_chain ref"),
        }
    }

    #[test]
    fn test_path_type_mismatch() {
        let json = json!({
            "data": "not_an_object"
        });

        let path = PathExpr::from_slice(&["data", "field"]);
        let result = extract_path(&path, &json);

        assert!(result.is_err());
        if let Err(DecodeError::TypeMismatch {
            expected, found, ..
        }) = result
        {
            assert_eq!(expected, "object");
            assert_eq!(found, "string");
        } else {
            panic!("Expected TypeMismatch error");
        }
    }

    #[test]
    fn relation_omitted_is_unspecified_not_empty() {
        let child_decoder =
            EntityDecoder::new("Issue", PathExpr::from_slice(&["children", "nodes", "*"]))
                .with_fields(vec![FieldDecoder::new("id", PathExpr::from_slice(&["id"]))]);
        let with_children = EntityDecoder::new("Issue", PathExpr::empty())
            .with_id_field("id")
            .with_fields(vec![FieldDecoder::new("id", PathExpr::from_slice(&["id"]))])
            .with_relations(vec![RelationDecoder {
                relation: "children".into(),
                decoder: child_decoder,
                cardinality: Cardinality::Many,
            }]);

        let issue = json!({
            "id": "i1",
            "children": { "nodes": [{ "id": "c1" }] }
        });
        let e = decode_entities(&with_children, &issue).unwrap();
        match e[0].relations.get("children").unwrap() {
            DecodedRelation::Specified(refs) => assert_eq!(refs.len(), 1),
            DecodedRelation::Unspecified => panic!("expected specified"),
        }

        let no_children_key = json!({ "id": "i1" });
        let e2 = decode_entities(&with_children, &no_children_key).unwrap();
        assert!(matches!(
            e2[0].relations.get("children").unwrap(),
            DecodedRelation::Unspecified
        ));
    }

    #[test]
    fn name_value_array_lookup_gmail_headers() {
        let headers = json!([
            {"name": "Subject", "value": "Hello"},
            {"name": "From", "value": "a@b.com"},
        ]);
        let rule = FieldDeriveRule::NameValueArrayLookup {
            equals: "From".into(),
            match_key_field: "name".into(),
            value_field: "value".into(),
            case_insensitive: true,
        };
        let out = apply_field_derive_rule(&rule, &headers).unwrap();
        assert_eq!(out, json!("a@b.com"));
    }

    #[test]
    fn name_value_array_lookup_case_insensitive_header_name() {
        let headers = json!([{"name": "from", "value": "x@y.com"}]);
        let rule = FieldDeriveRule::NameValueArrayLookup {
            equals: "From".into(),
            match_key_field: "name".into(),
            value_field: "value".into(),
            case_insensitive: true,
        };
        let out = apply_field_derive_rule(&rule, &headers).unwrap();
        assert_eq!(out, json!("x@y.com"));
    }

    #[test]
    fn name_value_array_lookup_aws_tags_key_value() {
        let tags = json!([
            {"Key": "Env", "Value": "prod"},
            {"Key": "Team", "Value": "plasm"},
        ]);
        let rule = FieldDeriveRule::NameValueArrayLookup {
            equals: "Team".into(),
            match_key_field: "Key".into(),
            value_field: "Value".into(),
            case_insensitive: false,
        };
        let out = apply_field_derive_rule(&rule, &tags).unwrap();
        assert_eq!(out, json!("plasm"));
    }

    #[test]
    fn name_value_array_lookup_missing_returns_null() {
        let headers = json!([{"name": "Subject", "value": "Hi"}]);
        let rule = FieldDeriveRule::NameValueArrayLookup {
            equals: "From".into(),
            match_key_field: "name".into(),
            value_field: "value".into(),
            case_insensitive: true,
        };
        let out = apply_field_derive_rule(&rule, &headers).unwrap();
        assert_eq!(out, serde_json::Value::Null);
    }

    #[test]
    fn object_key_lookup_object() {
        let obj = json!({"From": "a@b.com", "Subject": "S"});
        let rule = FieldDeriveRule::ObjectKeyLookup {
            key: "Subject".into(),
            case_insensitive: false,
        };
        let out = apply_field_derive_rule(&rule, &obj).unwrap();
        assert_eq!(out, json!("S"));
    }

    #[test]
    fn object_key_lookup_case_insensitive() {
        let obj = json!({"subject": "lowercase key"});
        let rule = FieldDeriveRule::ObjectKeyLookup {
            key: "Subject".into(),
            case_insensitive: true,
        };
        let out = apply_field_derive_rule(&rule, &obj).unwrap();
        assert_eq!(out, json!("lowercase key"));
    }

    #[test]
    fn colon_pair_prefer_keys_else_block_refs_from_blocks() {
        let state = json!({
            "baseToken": null,
            "blocks": [{"ref": "snapshot:snap1:mt1:abc123hash:b0"}]
        });
        let rule = FieldDeriveRule::ColonPairPreferKeysElseBlockRefs {
            prefer_keys: vec!["baseToken".into()],
            blocks_key: "blocks".into(),
            ref_field: "ref".into(),
            segment: "mt1".into(),
        };
        let out = apply_field_derive_rule(&rule, &state).unwrap();
        assert_eq!(out, json!("mt1:abc123hash"));
    }

    #[test]
    fn colon_pair_prefers_base_token_string_when_present() {
        let state = json!({
            "baseToken": "snapshot:x:mt1:direct:b1",
            "blocks": []
        });
        let rule = FieldDeriveRule::ColonPairPreferKeysElseBlockRefs {
            prefer_keys: vec!["baseToken".into()],
            blocks_key: "blocks".into(),
            ref_field: "ref".into(),
            segment: "mt1".into(),
        };
        let out = apply_field_derive_rule(&rule, &state).unwrap();
        assert_eq!(out, json!("mt1:direct"));
    }

    #[test]
    fn colon_pair_non_object_returns_null() {
        let rule = FieldDeriveRule::ColonPairPreferKeysElseBlockRefs {
            prefer_keys: vec![],
            blocks_key: "blocks".into(),
            ref_field: "ref".into(),
            segment: "mt1".into(),
        };
        let out = apply_field_derive_rule(&rule, &json!("nope")).unwrap();
        assert_eq!(out, serde_json::Value::Null);
    }

    #[test]
    fn decode_message_with_payload_headers_derive() {
        let json = json!({
            "id": "m1",
            "threadId": "t1",
            "payload": {
                "headers": [
                    {"name": "From", "value": "sender@example.com"},
                    {"name": "Subject", "value": "Title"},
                    {"name": "Date", "value": "Mon, 1 Jan 2024 00:00:00 +0000"}
                ]
            }
        });

        let decoder = EntityDecoder::new("Message", PathExpr::empty())
            .with_id_field("id")
            .with_fields(vec![
                FieldDecoder::new("id", PathExpr::from_slice(&["id"])),
                FieldDecoder::new("threadId", PathExpr::from_slice(&["threadId"])),
                FieldDecoder::new("headerFrom", PathExpr::from_slice(&["payload", "headers"]))
                    .with_derive(FieldDeriveRule::NameValueArrayLookup {
                        equals: "From".into(),
                        match_key_field: "name".into(),
                        value_field: "value".into(),
                        case_insensitive: true,
                    }),
                FieldDecoder::new(
                    "headerSubject",
                    PathExpr::from_slice(&["payload", "headers"]),
                )
                .with_derive(FieldDeriveRule::NameValueArrayLookup {
                    equals: "Subject".into(),
                    match_key_field: "name".into(),
                    value_field: "value".into(),
                    case_insensitive: true,
                }),
            ]);

        let entities = decode_entities(&decoder, &json).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(
            entities[0].fields.get("headerFrom"),
            Some(&Value::String("sender@example.com".into()))
        );
        assert_eq!(
            entities[0].fields.get("headerSubject"),
            Some(&Value::String("Title".into()))
        );
    }

    #[test]
    fn decode_objectid_hit_backfills_integer_id() {
        let json = json!({
            "hits": [
                {
                    "objectID": "42",
                    "title": "Hello",
                }
            ]
        });
        let decoder = EntityDecoder::new("Item", PathExpr::from_slice(&["hits", "*"]))
            .with_fields(vec![FieldDecoder::new(
                "title",
                PathExpr::from_slice(&["title"]),
            )])
            .with_id_field("id");
        let entities = decode_entities(&decoder, &json).unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].fields.get("id"), Some(&Value::Integer(42)));
        assert_eq!(
            entities[0].fields.get("title"),
            Some(&Value::String("Hello".to_string()))
        );
        assert_eq!(entities[0].reference.simple_id().unwrap().as_str(), "42");
    }
}
