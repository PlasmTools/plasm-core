//! Single-hop relation embed decode (CEP-10). No nested [`EntityDecoder::relations`].

use indexmap::IndexMap;
use plasm_core::{Ref, Value};
use std::collections::BTreeMap;

use crate::decoder::{
    apply_field_derive_rule, apply_transform, extract_path, json_to_value,
    relation_decode_path_specified, DecodedEntity, DecodedRelation, EntityDecoder,
};
use crate::DecodeError;

/// Decode entities using an [`EntityDecoder`].
pub fn decode_entities(
    decoder: &EntityDecoder,
    response: &serde_json::Value,
) -> Result<Vec<DecodedEntity>, DecodeError> {
    let source_values = extract_path(&decoder.source, response)?;
    let mut entities = Vec::with_capacity(source_values.len());
    for source_value in &source_values {
        entities.push(decode_single_entity(decoder, source_value)?);
    }
    Ok(entities)
}

fn decode_single_entity(
    decoder: &EntityDecoder,
    source: &serde_json::Value,
) -> Result<DecodedEntity, DecodeError> {
    let core = decode_entity_fields_and_ref(decoder, source)?;
    let mut relations = IndexMap::new();
    let mut embedded_entities = Vec::new();

    for relation_decoder in &decoder.relations {
        if !relation_decode_path_specified(source, &relation_decoder.decoder.source) {
            relations.insert(
                relation_decoder.relation.clone(),
                DecodedRelation::Unspecified,
            );
            continue;
        }
        if !relation_decoder.decoder.relations.is_empty() {
            return Err(DecodeError::InvalidStructure {
                message: format!(
                    "relation `{}` embed decoder must be single-hop (nested .relations forbidden)",
                    relation_decoder.relation
                ),
            });
        }
        let child_decoder = child_decoder_with_parent_ambient(source, &relation_decoder.decoder);
        let child_sources = extract_path(&child_decoder.source, source)?;
        let mut refs = Vec::new();
        for child_source in child_sources {
            let related = decode_entity_fields_and_ref(&child_decoder, &child_source)?;
            let reference = related.reference;
            refs.push(reference.clone());
            embedded_entities.push(DecodedEntity {
                reference,
                fields: related.fields,
                relations: IndexMap::new(),
                embedded_entities: Vec::new(),
            });
        }
        relations.insert(
            relation_decoder.relation.clone(),
            DecodedRelation::Specified(refs),
        );
    }

    Ok(DecodedEntity {
        reference: core.reference,
        fields: core.fields,
        relations,
        embedded_entities,
    })
}

struct DecodedEntityCore {
    reference: Ref,
    fields: IndexMap<String, Value>,
}

fn value_to_key_slot(v: &Value) -> Option<String> {
    match v {
        Value::PlasmInputRef(_) => None,
        Value::String(s) => Some(s.clone()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => {
            if f.is_finite() && f.fract() == 0.0 {
                Some((*f as i64).to_string())
            } else {
                Some(f.to_string())
            }
        }
        Value::Bool(b) => Some(b.to_string()),
        Value::Null | Value::Array(_) | Value::Object(_) | Value::UnionCtor { .. } => None,
    }
}

fn decode_entity_fields_and_ref(
    decoder: &EntityDecoder,
    source: &serde_json::Value,
) -> Result<DecodedEntityCore, DecodeError> {
    let mut fields = IndexMap::new();

    let id_value = if let Some(ref rid) = decoder.request_identity_override {
        rid.clone()
    } else if let Some(ref path) = decoder.id_path {
        let vals = extract_path(path, source)?;
        let first = vals.first().ok_or_else(|| DecodeError::InvalidStructure {
            message: "id_path matched no value".to_string(),
        })?;
        json_scalar_to_id_string(first)?
    } else {
        extract_id_from_source(source, decoder.id_field.as_deref())?
    };

    if source.is_object() {
        for field_decoder in &decoder.fields {
            let field_values = extract_path(&field_decoder.from, source)?;
            if let Some(first_value) = field_values.first() {
                let mut raw = first_value.clone();
                if let Some(ref dr) = field_decoder.derive {
                    raw = apply_field_derive_rule(dr, &raw)?;
                }
                let decoded_value = if let Some(transform) = &field_decoder.transform {
                    apply_transform(transform, &raw)?
                } else {
                    json_to_value(&raw)
                };
                fields.insert(field_decoder.field.clone(), decoded_value);
            }
        }
        if decoder.id_path.is_none() && decoder.request_identity_override.is_none() {
            if let Some(ref name) = decoder.id_field {
                if !fields.contains_key(name) {
                    fields.insert(name.clone(), value_for_id_field_from_string(&id_value));
                }
            }
        }
    } else if matches!(
        source,
        serde_json::Value::String(_) | serde_json::Value::Number(_)
    ) {
        if let Some(ref name) = decoder.id_field {
            fields.insert(name.clone(), json_to_value(source));
        }
    } else {
        return Err(DecodeError::InvalidStructure {
            message: "entity decode source must be a JSON object or a string/number id scalar"
                .to_string(),
        });
    }

    if decoder.id_path.is_some() || decoder.request_identity_override.is_some() {
        if let Some(ref name) = decoder.id_field {
            if !fields.contains_key(name) {
                fields.insert(name.clone(), Value::String(id_value.clone()));
            }
        }
    }

    let reference = build_decoded_reference(decoder, &fields, &id_value)?;
    Ok(DecodedEntityCore { reference, fields })
}

fn build_decoded_reference(
    decoder: &EntityDecoder,
    fields: &IndexMap<String, Value>,
    simple_id: &str,
) -> Result<Ref, DecodeError> {
    if decoder.key_vars.len() >= 2 {
        let mut parts = BTreeMap::new();
        for k in &decoder.key_vars {
            let v = fields
                .get(k)
                .and_then(value_to_key_slot)
                .or_else(|| decoder.identity_ambient.get(k).cloned())
                .ok_or_else(|| DecodeError::InvalidStructure {
                    message: format!(
                        "compound key part `{k}` missing for entity `{}` (row fields and identity ambient do not supply it)",
                        decoder.entity
                    ),
                })?;
            parts.insert(k.clone(), v);
        }
        Ok(Ref::compound(&decoder.entity, parts))
    } else if decoder.key_vars.len() == 1 {
        let k0 = decoder.key_vars[0].as_str();
        let v = fields
            .get(k0)
            .and_then(value_to_key_slot)
            .or_else(|| decoder.identity_ambient.get(k0).cloned())
            .unwrap_or_else(|| simple_id.to_string());
        Ok(Ref::new(&decoder.entity, v))
    } else {
        Ok(Ref::new(&decoder.entity, simple_id.to_string()))
    }
}

fn json_value_identity_slot_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Null | serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            None
        }
    }
}

fn child_decoder_with_parent_ambient(
    parent: &serde_json::Value,
    child: &EntityDecoder,
) -> EntityDecoder {
    let mut out = child.clone();
    if child.key_vars.len() < 2 {
        return out;
    }
    let Some(parent_obj) = parent.as_object() else {
        return out;
    };
    for kv in &child.key_vars {
        if out.identity_ambient.contains_key(kv) {
            continue;
        }
        if let Some(v) = parent_obj.get(kv.as_str()) {
            if let Some(s) = json_value_identity_slot_string(v) {
                out.identity_ambient.insert(kv.clone(), s);
            }
        }
    }
    for hint in &child.parent_identity_field_hints {
        if out.identity_ambient.contains_key(&hint.slot) {
            continue;
        }
        let Ok(vals) = extract_path(&hint.from, parent) else {
            continue;
        };
        let Some(first) = vals.first() else {
            continue;
        };
        let mut raw = first.clone();
        if let Some(ref dr) = hint.derive {
            if let Ok(derived) = apply_field_derive_rule(dr, &raw) {
                raw = derived;
            }
        }
        if let Ok(s) = json_scalar_to_id_string(&raw) {
            out.identity_ambient.insert(hint.slot.clone(), s);
        }
    }
    out
}

fn json_scalar_to_id_string(v: &serde_json::Value) -> Result<String, DecodeError> {
    match v {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        _ => Err(DecodeError::InvalidStructure {
            message: "id_path must resolve to a string or number".to_string(),
        }),
    }
}

fn value_for_id_field_from_string(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() {
        Value::Integer(i)
    } else {
        Value::String(s.to_string())
    }
}

fn extract_id_from_source(
    source: &serde_json::Value,
    schema_id_field: Option<&str>,
) -> Result<String, DecodeError> {
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(k) = schema_id_field.filter(|k| !k.is_empty()) {
        candidates.push(k);
    }
    for fb in ["id", "_id", "uuid", "key"] {
        if !candidates.contains(&fb) {
            candidates.push(fb);
        }
    }

    for field_name in candidates {
        if let Some(obj) = source.as_object() {
            if let Some(id_value) = obj.get(field_name) {
                return match id_value {
                    serde_json::Value::String(s) => Ok(s.clone()),
                    serde_json::Value::Number(n) => Ok(n.to_string()),
                    _ => continue,
                };
            }
        }
    }

    if let Some(obj) = source.as_object() {
        if let Some(oid) = obj.get("objectID") {
            match oid {
                serde_json::Value::String(s) => return Ok(s.clone()),
                serde_json::Value::Number(n) => return Ok(n.to_string()),
                _ => {}
            }
        }
    }

    match source {
        serde_json::Value::String(s) => Ok(s.clone()),
        serde_json::Value::Number(n) => Ok(n.to_string()),
        _ => Err(DecodeError::InvalidStructure {
            message: "No valid ID field found in source object".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::{PathExpr, RelationDecoder};
    use plasm_core::Cardinality;
    use serde_json::json;

    #[test]
    fn nested_embed_decoder_rejected() {
        let detail =
            EntityDecoder::new("LangDetail", PathExpr::from_slice(&["detail"])).with_id_field("id");
        let summary = EntityDecoder::new("LangSummary", PathExpr::from_slice(&["summary"]))
            .with_id_field("id")
            .with_relations(vec![RelationDecoder {
                relation: "detail".into(),
                decoder: detail,
                cardinality: Cardinality::One,
            }]);
        let item = EntityDecoder::new("LangItem", PathExpr::empty())
            .with_id_field("id")
            .with_relations(vec![RelationDecoder {
                relation: "summary".into(),
                decoder: summary,
                cardinality: Cardinality::One,
            }]);
        let body = json!({
            "id": "i1",
            "summary": { "id": "s1", "detail": { "id": "d1" } }
        });
        let err = decode_entities(&item, &body).unwrap_err();
        assert!(err.to_string().contains("single-hop"), "{err}");
    }
}
