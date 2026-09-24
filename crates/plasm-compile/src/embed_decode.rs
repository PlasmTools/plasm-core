//! Relation embed decode (CEP-10).
//!
//! Top-level [`EntityDecoder::relations`] are **leaf** embed decoders (no nested `.relations`).
//! When a [`CGS`] is supplied, nested `from_parent_get` chains are expanded iteratively up to
//! [`plasm_core::MAX_FROM_PARENT_GET_EMBED_DEPTH`] so chained plan hops (e.g. `summary.detail`) populate
//! the session graph without recursive stack growth.

use indexmap::IndexMap;
use plasm_core::{Ref, RelationMaterialization, Value, CGS, MAX_FROM_PARENT_GET_EMBED_DEPTH};
use std::collections::{BTreeMap, VecDeque};

use crate::decoder::{
    apply_field_derive_rule, apply_transform, extract_path, relation_decode_path_specified,
    DecodedEntity, DecodedRelation, EntityDecoder,
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
    let core = decode_entity_fields_and_ref(decoder, source, cgs)?;
    let mut relations = IndexMap::new();
    let mut embedded_entities = Vec::new();
    let field_diagnostics = core.field_diagnostics;

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
        let child_decoder =
            child_decoder_with_parent_ambient(&core.fields, &relation_decoder.decoder);
        let child_sources = extract_path(&child_decoder.source, source)?;
        let mut refs = Vec::new();
        for child_source in child_sources {
            let related = decode_entity_fields_and_ref(&child_decoder, &child_source, cgs)?;
            let reference = related.reference;
            refs.push(reference.clone());
            let mut child_entity = DecodedEntity {
                reference,
                fields: related.fields,
                relations: IndexMap::new(),
                embedded_entities: Vec::new(),
                field_diagnostics: related.field_diagnostics,
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
        field_diagnostics,
    })
}

struct DecodedEntityCore {
    reference: Ref,
    fields: IndexMap<String, Value>,
    field_diagnostics: Vec<plasm_core::DecodeFieldDiagnostic>,
}

fn value_to_key_slot(v: &Value) -> Option<String> {
    match v {
        Value::PlasmInputRef(_) | Value::GetScalarExtract(_) | Value::StringTemplate(_) => None,
        Value::String(s) | Value::PhraseIdent(s) => Some(s.clone()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => {
            if f.is_finite() && f.fract() == 0.0 {
                Some((*f as i64).to_string())
            } else {
                Some(f.to_string())
            }
        }
        Value::Bool(b) => Some(b.to_string()),
        Value::Money(m) => m.to_wire_text().ok(),
        Value::Null | Value::Array(_) | Value::Object(_) | Value::UnionCtor { .. } => None,
    }
}

fn decode_entity_fields_and_ref(
    decoder: &EntityDecoder,
    source: &serde_json::Value,
    cgs: Option<&CGS>,
) -> Result<DecodedEntityCore, DecodeError> {
    let mut fields = IndexMap::new();
    let mut field_diagnostics = Vec::new();
    let entity_def = cgs.and_then(|c| c.get_entity(decoder.entity.as_str()));

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
                let mut decoded_value = if field_decoder.money.is_some() {
                    plasm_core::json_amount_to_value(&raw)
                } else if let Some(transform) = &field_decoder.transform {
                    apply_transform(transform, &raw)?
                } else {
                    plasm_core::json_value_to_plasm_value(&raw)
                };
                if field_decoder.money.is_none() {
                    if let (Some(cgs), Some(ent)) = (cgs, entity_def) {
                        if let Some(fs) = ent.fields.get(field_decoder.field.as_str()) {
                            if let Ok(nv) = fs.named_value(cgs) {
                                let (v, diag) = plasm_core::decode_coerce_and_validate_field(
                                    field_decoder.field.as_str(),
                                    nv,
                                    decoded_value,
                                );
                                decoded_value = v;
                                if let Some(d) = diag {
                                    field_diagnostics.push(d);
                                }
                            }
                        }
                    }
                }
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
            fields.insert(name.clone(), plasm_core::json_value_to_plasm_value(source));
        }
    } else {
        return Err(DecodeError::InvalidStructure {
            message: "entity decode source must be a JSON object or a string/number id scalar"
                .to_string(),
        });
    }

    if decoder.id_path.is_some() || decoder.request_identity_override.is_some() {
        if let Some(ref name) = decoder.id_field {
            if fields
                .get(name)
                .is_none_or(|value| matches!(value, Value::Null))
            {
                let raw = Value::String(id_value.clone());
                let value = if let (Some(cgs), Some(entity)) = (cgs, entity_def) {
                    let schema = entity
                        .fields
                        .get(name.as_str())
                        .and_then(|field| field.named_value(cgs).ok())
                        .ok_or_else(|| DecodeError::InvalidStructure {
                            message: format!("identity field `{name}` has no declared value type"),
                        })?;
                    let (value, diagnostic) =
                        plasm_core::decode_coerce_and_validate_field(name, schema, raw);
                    if let Some(diagnostic) = diagnostic {
                        return Err(DecodeError::InvalidStructure {
                            message: diagnostic.message,
                        });
                    }
                    value
                } else {
                    raw
                };
                fields.insert(name.clone(), value);
            }
        }
    }

    let money_specs: Vec<_> = decoder
        .fields
        .iter()
        .filter_map(|fd| fd.money.clone().map(|spec| (fd.field.clone(), spec)))
        .collect();
    if !money_specs.is_empty() {
        plasm_core::decode_coerce_money_fields(&mut fields, money_specs, &mut field_diagnostics);
    }

    // Parent-scoped identity components are also observable typed fields.
    // Materialize only declared key slots; never infer types from their spelling.
    for key in &decoder.key_vars {
        if fields
            .get(key)
            .is_some_and(|value| !matches!(value, Value::Null))
        {
            continue;
        }
        let Some(slot) = decoder.identity_ambient.get(key) else {
            continue;
        };
        let raw = Value::String(slot.clone());
        let value = if let (Some(cgs), Some(entity)) = (cgs, entity_def) {
            let schema = entity
                .fields
                .get(key.as_str())
                .and_then(|field| field.named_value(cgs).ok())
                .ok_or_else(|| DecodeError::InvalidStructure {
                    message: format!("identity field `{key}` has no declared value type"),
                })?;
            let (value, diagnostic) =
                plasm_core::decode_coerce_and_validate_field(key, schema, raw);
            if let Some(diagnostic) = diagnostic {
                return Err(DecodeError::InvalidStructure {
                    message: diagnostic.message,
                });
            }
            value
        } else {
            raw
        };
        fields.insert(key.clone(), value);
    }

    let reference = build_decoded_reference(decoder, &fields, &id_value)?;
    Ok(DecodedEntityCore {
        reference,
        fields,
        field_diagnostics,
    })
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

fn child_decoder_with_parent_ambient(
    parent_fields: &IndexMap<String, Value>,
    child: &EntityDecoder,
) -> EntityDecoder {
    let mut out = child.clone();
    if child.key_vars.len() < 2 {
        return out;
    }
    for binding in &child.parent_identity_bindings {
        if out.identity_ambient.contains_key(&binding.slot) {
            continue;
        }
        if let Some(slot) = parent_fields
            .get(&binding.parent_field)
            .and_then(value_to_key_slot)
        {
            out.identity_ambient.insert(binding.slot.clone(), slot);
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
            let child_decoder = child_decoder_with_parent_ambient(&entity.fields, &child_decoder);
            let child_sources = extract_path(&rel_path, &wire)?;
            let mut refs = Vec::new();
            for child_wire in child_sources {
                let related = decode_entity_fields_and_ref(&child_decoder, &child_wire, Some(cgs))?;
                let reference = related.reference;
                refs.push(reference.clone());
                let child_idx = entity.embedded_entities.len();
                entity.embedded_entities.push(DecodedEntity {
                    reference,
                    fields: related.fields,
                    relations: IndexMap::new(),
                    embedded_entities: Vec::new(),
                    field_diagnostics: related.field_diagnostics,
                });
                let mut child_path = path.clone();
                child_path.push(child_idx);
                queue.push_back((child_path, child_wire, target_type.to_string(), depth + 1));
            }
            entity.relations.insert(
                rel_name.as_str().to_string(),
                DecodedRelation::Specified(refs),
            );
        }
    }
    Ok(())
}

fn entity_at_embed_path_mut<'a>(
    root: &'a mut DecodedEntity,
    path: &[usize],
) -> &'a mut DecodedEntity {
    let mut cur = root;
    for &idx in path {
        cur = &mut cur.embedded_entities[idx];
    }
    cur
}

/// Extract the wire identity using the same conventions for top-level and embedded rows.
pub fn extract_id_from_source(
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

    proptest::proptest! {
        #[test]
        fn parent_identity_fields_preserve_declared_types_across_codec(
            item_id in 0i64..1_000_000,
            owner in "[0-9]{1,8}",
            explicit_null in proptest::bool::ANY,
        ) {
            let mut cgs = plasm_core::load_schema_dir(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix")).unwrap();
            cgs.values.get_mut("nv_compound_branch_item_id").unwrap().field_type=plasm_core::FieldType::Integer;
            let entity=cgs.get_entity("CompoundBranch").unwrap();
            let decoder=crate::embed_target_decoder::entity_decoder_for_from_parent_get_target(
                entity,entity,PathExpr::empty()).with_identity_ambient(IndexMap::from([
                    ("owner".into(),owner.clone()),("item_id".into(),item_id.to_string())]));
            let packed:EntityDecoder=serde_json::from_slice(&serde_json::to_vec(&decoder).unwrap()).unwrap();
            let mut body=json!({"name":"branch","color":"green"});
            if explicit_null { body["item_id"]=serde_json::Value::Null; }
            let decoded=decode_entities_with_cgs(&packed,&body,Some(&cgs)).unwrap();
            proptest::prop_assert_eq!(&decoded[0].fields["owner"],&Value::String(owner));
            proptest::prop_assert_eq!(&decoded[0].fields["item_id"],&Value::Integer(item_id));
            let original=decode_entities_with_cgs(&decoder,&body,Some(&cgs)).unwrap();
            proptest::prop_assert_eq!(&decoded[0].reference,&original[0].reference);
            proptest::prop_assert_eq!(&decoded[0].fields,&original[0].fields);
            // Parent identity can originate in the request, with no wire identity at all.
            // Every embedding level inherits the already decoded, typed slots.
            let child=crate::embed_target_decoder::entity_decoder_for_from_parent_get_target(
                entity,entity,PathExpr::empty());
            let inherited=child_decoder_with_parent_ambient(&decoded[0].fields,&child);
            let inherited:EntityDecoder=serde_json::from_slice(&serde_json::to_vec(&inherited).unwrap()).unwrap();
            let nested=decode_entities_with_cgs(&inherited,&body,Some(&cgs)).unwrap();
            proptest::prop_assert_eq!(&nested[0].reference,&original[0].reference);
            proptest::prop_assert_eq!(&nested[0].fields,&original[0].fields);
        }
    }

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
        let decoded = decode_entities_with_cgs(&item, &body, Some(&cgs))
            .expect("decode with transitive embed");
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

    proptest::proptest! {
        #[test]
        fn transitive_collection_codec_preserves_order_duplicates_and_empty(ids in proptest::collection::vec(0u8..5, 0..12)) {
            let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix");
            let mut cgs = plasm_core::loader::load_schema_dir(&dir).unwrap();
            let relation = cgs.entities.get_mut("LangSummary").unwrap().relations.get_mut("detail").unwrap();
            relation.cardinality = Cardinality::Many;
            relation.materialize = Some(serde_json::from_value(json!({
                "kind":"from_parent_get", "path":[{"key":"detail"},{"wildcard":true}]
            })).unwrap());
            let cgs = serde_json::from_slice(&serde_json::to_vec(&cgs).unwrap()).unwrap();
            let decoder = EntityDecoder::new("LangItem", PathExpr::empty()).with_id_field("id")
                .with_relations(vec![RelationDecoder {
                    relation: "summary".into(),
                    decoder: EntityDecoder::new("LangSummary", PathExpr::from_slice(&["summary"])).with_id_field("id"),
                    cardinality: Cardinality::One,
                }]);
            let decoder = serde_json::from_slice(&serde_json::to_vec(&decoder).unwrap()).unwrap();
            let children: Vec<_> = ids.iter().map(|id| json!({"id":format!("d{id}"),"body":"detail"})).collect();
            let body = json!({"id":"i1","summary":{"id":"s1","detail":children}});
            let rows = decode_entities_with_cgs(&decoder,&body,Some(&cgs)).unwrap();
            let summary = &rows[0].embedded_entities[0];
            let expected: Vec<_> = ids.iter().map(|id| Ref::new("LangDetail",format!("d{id}"))).collect();
            proptest::prop_assert_eq!(summary.relations.get("detail"),Some(&DecodedRelation::Specified(expected)));
            proptest::prop_assert_eq!(summary.embedded_entities.len(),ids.len());
            let omitted = json!({"id":"i1","summary":{"id":"s1"}});
            let rows = decode_entities_with_cgs(&decoder,&omitted,Some(&cgs)).unwrap();
            proptest::prop_assert!(!matches!(rows[0].embedded_entities[0].relations.get("detail"),Some(DecodedRelation::Specified(_))));
        }
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
