//! CGS `from_parent_get` embed target decoders (CEP-10: leaf decoders have no nested `.relations`).

use indexmap::IndexMap;

use crate::decoder::{EntityDecoder, FieldDecoder, ParentIdentityBinding, PathExpr, PathSegment};

/// Prove the declared identity projection for every embedded relation before packing.
/// Wire presence is a backend contract; source paths and inherited slot types are ours.
pub(crate) fn validate_embedded_identity_contracts(
    cgs: &plasm_core::CGS,
) -> Result<(), crate::CmlError> {
    for parent in cgs.entities.values() {
        for (name, relation) in &parent.relations {
            if !matches!(
                relation.materialize,
                Some(
                    plasm_core::RelationMaterialization::FromParentGet { .. }
                        | plasm_core::RelationMaterialization::PreferFromParentGet { .. }
                )
            ) {
                continue;
            }
            let target = cgs
                .get_entity(relation.target_resource.as_str())
                .ok_or_else(|| crate::CmlError::InvalidTemplate {
                    message: format!(
                        "{}.{}: missing embedded entity {}",
                        parent.name, name, relation.target_resource
                    ),
                })?;
            for key in target
                .key_vars
                .iter()
                .map(|key| key.as_str())
                .chain(std::iter::once(target.id_field.as_str()))
            {
                let child =
                    target
                        .fields
                        .get(key)
                        .ok_or_else(|| crate::CmlError::InvalidTemplate {
                            message: format!(
                                "{}.{}: embedded identity slot {}.{key} has no declared field",
                                parent.name, name, target.name
                            ),
                        })?;
                let child_type =
                    child
                        .named_value(cgs)
                        .map_err(|error| crate::CmlError::InvalidTemplate {
                            message: error.to_string(),
                        })?;
                if child.wire_path.as_ref().is_some_and(|path| path.is_empty()) {
                    return Err(crate::CmlError::InvalidTemplate {
                        message: format!(
                            "{}.{}: identity slot {}.{key} has an empty wire path",
                            parent.name, name, target.name
                        ),
                    });
                }
                if target.key_vars.len() > 1 && key != target.id_field.as_str() {
                    if let Some(source) = parent.fields.get(key) {
                        let source_type = source.named_value(cgs).map_err(|error| {
                            crate::CmlError::InvalidTemplate {
                                message: error.to_string(),
                            }
                        })?;
                        if source_type.field_type != child_type.field_type
                            || source_type.value_format != child_type.value_format
                        {
                            return Err(crate::CmlError::InvalidTemplate { message: format!("{}.{}: inherited identity slot {key} has incompatible parent and child types", parent.name, name) });
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Build a single-hop embed decoder for a `from_parent_get` relation target (no nested `.relations`).
pub fn entity_decoder_for_from_parent_get_target(
    target_ent: &plasm_core::EntityDef,
    parent_ent: &plasm_core::EntityDef,
    rel_path: PathExpr,
) -> EntityDecoder {
    let mut cf = Vec::new();
    for (fname, fschema) in &target_ent.fields {
        let from_path = if let Some(wp) = &fschema.wire_path {
            PathExpr::new(
                wp.iter()
                    .map(|n| PathSegment::Key { name: n.clone() })
                    .collect(),
            )
        } else {
            PathExpr::new(vec![PathSegment::Key {
                name: fname.as_str().to_string(),
            }])
        };
        let fd = FieldDecoder::new(fname.as_str(), from_path);
        cf.push(match &fschema.derive {
            Some(d) => fd.with_derive(d.clone()),
            None => fd,
        });
    }
    let child_kv: Vec<String> = target_ent
        .key_vars
        .iter()
        .map(|k| k.as_str().to_string())
        .collect();
    let parent_bindings = parent_identity_bindings_for_child(parent_ent, target_ent);
    let mut decoder = EntityDecoder::new(target_ent.name.as_str(), rel_path)
        .with_fields(cf)
        .with_id_field(target_ent.id_field.clone())
        .with_key_vars(child_kv)
        .with_identity_ambient(IndexMap::new())
        .with_parent_identity_bindings(parent_bindings);
    if let Some(path) = target_ent
        .fields
        .get(target_ent.id_field.as_str())
        .and_then(|field| field.wire_path.as_ref())
    {
        decoder.id_path = Some(PathExpr::new(
            path.iter()
                .map(|name| PathSegment::Key { name: name.clone() })
                .collect(),
        ));
    }
    debug_assert!(
        decoder.relations.is_empty(),
        "from_parent_get target decoder must not nest relation embed decoders (CEP-10)"
    );
    decoder
}

fn parent_identity_bindings_for_child(
    parent_ent: &plasm_core::EntityDef,
    child_ent: &plasm_core::EntityDef,
) -> Vec<ParentIdentityBinding> {
    let mut bindings = Vec::new();
    for kv in &child_ent.key_vars {
        if kv.as_str() == child_ent.id_field.as_str() {
            continue;
        }
        let Some(_) = parent_ent.fields.get(kv.as_str()) else {
            continue;
        };
        bindings.push(ParentIdentityBinding {
            slot: kv.as_str().to_string(),
            parent_field: kv.as_str().to_string(),
        });
    }
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::loader::load_schema_dir;

    proptest::proptest! {
        #[test]
        fn nested_identity_path_survives_decoder_codec(id in "[a-z][a-z0-9]{0,20}") {
            let mut cgs = load_schema_dir(&std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix")).unwrap();
            cgs.entities.get_mut("LangLine").unwrap().fields.get_mut("id").unwrap().wire_path =
                Some(vec!["identity".into(), "value".into()]);
            validate_embedded_identity_contracts(&cgs).unwrap();
            let decoder = entity_decoder_for_from_parent_get_target(
                cgs.get_entity("LangLine").unwrap(), cgs.get_entity("LangItem").unwrap(), PathExpr::empty());
            let packed: EntityDecoder = serde_json::from_slice(&serde_json::to_vec(&decoder).unwrap()).unwrap();
            let wire = serde_json::json!({"identity":{"value":id},"item_id":"parent","note":"embedded"});
            let rows = crate::decode_entities_with_cgs(&packed, &wire, Some(&cgs)).unwrap();
            proptest::prop_assert_eq!(rows[0].reference.primary_slot_str(), id.as_str());
            proptest::prop_assert_eq!(&rows[0].fields["id"], &plasm_core::Value::String(id));
        }
    }

    #[test]
    fn obsolete_identity_hint_codec_is_rejected() {
        let decoder = EntityDecoder::new("Child", PathExpr::empty());
        let mut wire = serde_json::to_value(decoder).unwrap();
        wire.as_object_mut()
            .unwrap()
            .insert("parent_identity_field_hints".into(), serde_json::json!([]));
        assert!(serde_json::from_value::<EntityDecoder>(wire).is_err());
    }

    #[test]
    fn malformed_embedded_identity_is_rejected_before_execution() {
        let cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        let mut missing = cgs.clone();
        missing
            .entities
            .get_mut("LangLine")
            .unwrap()
            .fields
            .shift_remove("id");
        assert!(validate_embedded_identity_contracts(&missing)
            .unwrap_err()
            .to_string()
            .contains("no declared field"));
        let mut empty = cgs;
        empty
            .entities
            .get_mut("LangLine")
            .unwrap()
            .fields
            .get_mut("id")
            .unwrap()
            .wire_path = Some(vec![]);
        assert!(validate_embedded_identity_contracts(&empty)
            .unwrap_err()
            .to_string()
            .contains("empty wire path"));
    }

    #[test]
    fn inherited_identity_type_mismatch_is_rejected_before_execution() {
        let mut cgs = load_schema_dir(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/plasm_language_matrix"),
        )
        .unwrap();
        let parent = cgs.entities.get("LangItem").unwrap();
        let mut numeric = parent.fields["id"].named_value(&cgs).unwrap().clone();
        numeric.field_type = plasm_core::FieldType::Integer;
        cgs.values
            .insert("incompatible_child_identity".into(), numeric);
        let mut parent_slot = cgs.entities["LangItem"].fields["id"].clone();
        parent_slot.name = "item_id".into();
        cgs.entities
            .get_mut("LangItem")
            .unwrap()
            .fields
            .insert("item_id".into(), parent_slot);
        let child = cgs.entities.get_mut("LangLine").unwrap();
        child.key_vars = vec!["item_id".into(), "id".into()];
        child.fields.get_mut("item_id").unwrap().kind = plasm_core::FieldValueKind::Registry(
            plasm_core::ValueDomainKey::new("incompatible_child_identity").unwrap(),
        );
        let error = validate_embedded_identity_contracts(&cgs)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("incompatible parent and child types"),
            "{error}"
        );
        assert!(crate::compile_cgs_capability_templates(&cgs).is_err());
    }

    #[test]
    fn from_parent_get_target_decoders_have_no_nested_relations() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("langmatrix");
        let item = cgs.get_entity("LangItem").expect("LangItem");
        let summary = cgs.get_entity("LangSummary").expect("LangSummary");
        let path = PathExpr::from_slice(&["summary"]);
        let decoder = entity_decoder_for_from_parent_get_target(summary, item, path);
        assert!(decoder.relations.is_empty());
    }
}
