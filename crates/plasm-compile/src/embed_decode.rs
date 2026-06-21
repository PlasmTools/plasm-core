//! Relation embed decode (CEP-10).
//!
//! Top-level [`EntityDecoder::relations`] are **leaf** embed decoders (no nested `.relations`).
//! When a [`CGS`] is supplied, nested `from_parent_get` chains are expanded iteratively up to
//! [`plasm_core::MAX_FROM_PARENT_GET_EMBED_DEPTH`] so chained plan hops (e.g. `summary.detail`) populate
//! the session graph without recursive stack growth.

use indexmap::IndexMap;
use plasm_core::{CGS, MAX_FROM_PARENT_GET_EMBED_DEPTH, Ref, RelationMaterialization, Value};
use std::collections::{BTreeMap, VecDeque};

use crate::decoder::{
    apply_field_derive_rule, apply_transform, extract_path, json_to_value,
    relation_decode_path_specified, DecodedEntity, DecodedRelation, EntityDecoder,
};
use crate::embed_target_decoder::entity_decoder_for_from_parent_get_target;
use crate::json_path::path_expr_from_json_segments;
use crate::DecodeError;

/// Decode entities using an [`EntityDecoder`].
pub fn decode_entities(
    decoder: &EntityDecoder,
    response: &serde_json::Value,
) -> Result<Vec<DecodedEntity>, DecodeError> {
    decode_entities_with_cgs(decoder, response, None)
}

/// Decode entities and, when `cgs` is set, iteratively expand nested `from_parent_get` embeds.
pub fn decode_entities_with_cgs(
    decoder: &EntityDecoder,
    response: &serde_json::Value,
    cgs: Option<&CGS>,
) -> Result<Vec<DecodedEntity>, DecodeError> {
    let source_values = extract_path(&decoder.source, response)?;
    let mut entities = Vec::with_capacity(source_values.len());
    for source_value in &source_values {
        entities.push(decode_single_entity(decoder, source_value, cgs)?);
    }
    Ok(entities)
}

fn decode_single_entity(
    decoder: &EntityDecoder,
    source: &serde_json::Value,
    cgs: Option<&CGS>,
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
                    "relation `{}` embed decoder must be a leaf (nested .relations forbidden; CEP-10)",
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
            let mut child_entity = DecodedEntity {
                reference,
                fields: related.fields,
                relations: IndexMap::new(),
                embedded_entities: Vec::new(),
            };
            if let Some(cgs) = cgs {
                expand_transitive_from_parent_get_embeds(
                    &mut child_entity,
                    &child_source,
                    child_decoder.entity.as_str(),
                    cgs,
                    MAX_FROM_PARENT_GET_EMBED_DEPTH,
                )?;
            }
            embedded_entities.push(child_entity);
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

fn expand_transitive_from_parent_get_embeds(
    root: &mut DecodedEntity,
    root_wire: &serde_json::Value,
    root_type: &str,
    cgs: &CGS,
    max_depth: usize,
) -> Result<(), DecodeError> {
    let mut queue: VecDeque<(Vec<usize>, serde_json::Value, String, usize)> = VecDeque::new();
    queue.push_back((Vec::new(), root_wire.clone(), root_type.to_string(), 0));

    while let Some((path, wire, ent_type, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let entity = entity_at_embed_path_mut(root, &path);
        let Some(def) = cgs.get_entity(ent_type.as_str()) else {
            continue;
        };
        for (rel_name, rel_schema) in &def.relations {
            let path_seg = match &rel_schema.materialize {
                Some(RelationMaterialization::FromParentGet { path })
                | Some(RelationMaterialization::PreferFromParentGet { path, .. }) => path,
                _ => continue,
            };
            let rel_path = path_expr_from_json_segments(path_seg).map_err(|e| {
                DecodeError::InvalidStructure {
                    message: e.to_string(),
                }
            })?;
            if !relation_decode_path_specified(&wire, &rel_path) {
                continue;
            }
            let target_type = rel_schema.target_resource.as_str();
            let Some(target_ent) = cgs.get_entity(target_type) else {
                continue;
            };
            let child_decoder =
                entity_decoder_for_from_parent_get_target(target_ent, def, rel_path.clone());
            let child_sources = extract_path(&rel_path, &wire)?;
            let mut refs = Vec::new();
            for child_wire in child_sources {
                let related = decode_entity_fields_and_ref(&child_decoder, &child_wire)?;
                let reference = related.reference;
                refs.push(reference.clone());
                let child_idx = entity.embedded_entities.len();
                entity.embedded_entities.push(DecodedEntity {
                    reference,
                    fields: related.fields,
                    relations: IndexMap::new(),
                    embedded_entities: Vec::new(),
                });
                let mut child_path = path.clone();
                child_path.push(child_idx);
                queue.push_back((child_path, child_wire, target_type.to_string(), depth + 1));
            }
            if !refs.is_empty() {
                entity.relations.insert(rel_name.as_str().to_string(), DecodedRelation::Specified(refs));
            }
        }
    }
    Ok(())
}

fn entity_at_embed_path_mut<'a>(root: &'a mut DecodedEntity, path: &[usize]) -> &'a mut DecodedEntity {
    let mut cur = root;
    for &idx in path {
        cur = &mut cur.embedded_entities[idx];
    }
    cur
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
    fn transitive_from_parent_get_expand_with_cgs() {
        use plasm_core::loader::load_schema_dir;

        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("langmatrix");
        let summary = EntityDecoder::new("LangSummary", PathExpr::from_slice(&["summary"]))
            .with_id_field("id");
        let item = EntityDecoder::new("LangItem", PathExpr::empty())
            .with_id_field("id")
            .with_relations(vec![RelationDecoder {
                relation: "summary".into(),
                decoder: summary,
                cardinality: Cardinality::One,
            }]);
        let body = json!({
            "id": "i1",
            "summary": {
                "id": "sum-i1",
                "headline": "Alpha summary",
                "detail": { "id": "det-i1", "body": "nested detail" }
            }
        });
        let decoded =
            decode_entities_with_cgs(&item, &body, Some(&cgs)).expect("decode with transitive embed");
        let summary = decoded[0]
            .embedded_entities
            .iter()
            .find(|e| e.reference.entity_type.as_str() == "LangSummary")
            .expect("summary embed");
        let detail_rel = summary.relations.get("detail").expect("detail relation");
        assert!(matches!(detail_rel, DecodedRelation::Specified(_)));
        assert!(
            summary
                .embedded_entities
                .iter()
                .any(|e| e.reference.primary_slot_str() == "det-i1"),
            "detail entity must be in embedded_entities"
        );
    }

    #[test]
    fn nested_embed_decoder_on_entity_decoder_rejected() {
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
        assert!(err.to_string().contains("leaf"), "{err}");
    }
}
