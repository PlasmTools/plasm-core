//! CGS-driven entity decoder construction (CEP-10 single-hop embed decoders).

use indexmap::IndexMap;
use plasm_compile::{
    path_expr_from_json_segments, EntityDecoder, FieldDecoder, ParentIdentityFieldHint, PathExpr,
    PathSegment, RelationDecoder,
};
use plasm_core::{Cardinality, RelationMaterialization, CGS};

pub(crate) fn create_entity_decoder_for_capability(
    declared_entity: &str,
    cgs: &CGS,
    capability_name: Option<&str>,
    collection_source: Option<PathExpr>,
    request_identity: Option<&str>,
    identity_ambient: Option<&IndexMap<String, String>>,
) -> EntityDecoder {
    let entity_type = capability_name
        .and_then(|cap| resolve_overlay_decode_entity(cgs, cap, identity_ambient))
        .unwrap_or_else(|| declared_entity.to_string());
    create_entity_decoder_inner(
        &entity_type,
        cgs,
        collection_source,
        request_identity,
        identity_ambient,
    )
}

pub(crate) fn create_entity_decoder(
    entity_type: &str,
    cgs: &CGS,
    collection_source: Option<PathExpr>,
    request_identity: Option<&str>,
    identity_ambient: Option<&IndexMap<String, String>>,
) -> EntityDecoder {
    create_entity_decoder_for_capability(
        entity_type,
        cgs,
        None,
        collection_source,
        request_identity,
        identity_ambient,
    )
}

pub(crate) fn mutating_capability_response_decoder(
    entity_type: &str,
    capability_name: &str,
    cgs: &CGS,
    identity_ambient: &IndexMap<String, String>,
    request_identity: Option<&str>,
) -> EntityDecoder {
    create_entity_decoder_for_capability(
        entity_type,
        cgs,
        Some(capability_name),
        None,
        request_identity,
        Some(identity_ambient),
    )
}

pub(crate) fn resolve_overlay_decode_entity(
    cgs: &CGS,
    capability_name: &str,
    identity_ambient: Option<&IndexMap<String, String>>,
) -> Option<String> {
    let spec = cgs.schema_overlay.as_ref()?;
    let listed = if spec.decode.capabilities.is_empty() {
        capability_name == "entity_query" || capability_name == "entity_get"
    } else {
        spec.decode
            .capabilities
            .iter()
            .any(|c| c == capability_name)
    };
    if !listed {
        return None;
    }
    let ambient = identity_ambient?;
    let scope_value =
        plasm_core::schema_overlay::build_decode_scope_key(&spec.decode.scope, ambient)?;
    cgs.schema_overlay_scope_index
        .get(scope_value.as_str())
        .map(|n| n.to_string())
}

fn parent_identity_field_hints_for_child(
    parent_ent: &plasm_core::EntityDef,
    child_ent: &plasm_core::EntityDef,
) -> Vec<ParentIdentityFieldHint> {
    let mut hints = Vec::new();
    for kv in &child_ent.key_vars {
        let Some(fs) = parent_ent.fields.get(kv.as_str()) else {
            continue;
        };
        let from = if let Some(wp) = &fs.wire_path {
            PathExpr::new(
                wp.iter()
                    .map(|n| PathSegment::Key { name: n.clone() })
                    .collect(),
            )
        } else {
            PathExpr::new(vec![PathSegment::Key {
                name: kv.as_str().to_string(),
            }])
        };
        hints.push(ParentIdentityFieldHint {
            slot: kv.as_str().to_string(),
            from,
            derive: fs.derive.clone(),
        });
    }
    hints
}

fn entity_decoder_for_from_parent_get_target(
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
    let parent_hints = parent_identity_field_hints_for_child(parent_ent, target_ent);
    let decoder = EntityDecoder::new(target_ent.name.as_str(), rel_path)
        .with_fields(cf)
        .with_id_field(target_ent.id_field.clone())
        .with_key_vars(child_kv)
        .with_identity_ambient(IndexMap::new())
        .with_parent_identity_field_hints(parent_hints);
    debug_assert!(
        decoder.relations.is_empty(),
        "from_parent_get target decoder must not nest relation embeds"
    );
    decoder
}

fn create_entity_decoder_inner(
    entity_type: &str,
    cgs: &CGS,
    collection_source: Option<PathExpr>,
    request_identity: Option<&str>,
    identity_ambient: Option<&IndexMap<String, String>>,
) -> EntityDecoder {
    let ambient = identity_ambient.cloned().unwrap_or_default();

    let source = match collection_source {
        Some(p) => p,
        None => PathExpr::empty(),
    };

    let mut field_decoders = Vec::new();

    if let Some(entity) = cgs.get_entity(entity_type) {
        for (field_name, field_schema) in &entity.fields {
            let from_path = if let Some(wp) = &field_schema.wire_path {
                PathExpr::new(
                    wp.iter()
                        .map(|n| PathSegment::Key { name: n.clone() })
                        .collect(),
                )
            } else {
                PathExpr::new(vec![PathSegment::Key {
                    name: field_name.as_str().to_string(),
                }])
            };
            let fd = FieldDecoder::new(field_name.as_str(), from_path);
            field_decoders.push(match &field_schema.derive {
                Some(d) => fd.with_derive(d.clone()),
                None => fd,
            });
        }
        for (rel_name, rel) in &entity.relations {
            if rel.cardinality == Cardinality::One {
                let Some(target_ent) = cgs.get_entity(rel.target_resource.as_str()) else {
                    continue;
                };
                let nested_key = target_ent.id_field.clone();
                field_decoders.push(FieldDecoder::new(
                    rel_name.as_str(),
                    PathExpr::new(vec![
                        PathSegment::Key {
                            name: rel_name.as_str().to_string(),
                        },
                        PathSegment::Key {
                            name: nested_key.into(),
                        },
                    ]),
                ));
            }
        }
    } else {
        field_decoders.push(FieldDecoder::new(
            "id",
            PathExpr::new(vec![PathSegment::Key {
                name: "id".to_string(),
            }]),
        ));
    }

    let mut relation_decoders: Vec<RelationDecoder> = Vec::new();
    if let Some(entity) = cgs.get_entity(entity_type) {
        for (rel_name, rel) in &entity.relations {
            if let Some(path) = match &rel.materialize {
                Some(RelationMaterialization::FromParentGet { path })
                | Some(RelationMaterialization::PreferFromParentGet { path, .. }) => Some(path),
                _ => None,
            } {
                let rel_path = path_expr_from_json_segments(path).unwrap_or_else(|e| {
                    panic!("CGS must reject invalid from_parent_get paths: {e}");
                });
                let Some(target_ent) = cgs.get_entity(rel.target_resource.as_str()) else {
                    continue;
                };
                let child = entity_decoder_for_from_parent_get_target(target_ent, entity, rel_path);
                relation_decoders.push(RelationDecoder {
                    relation: rel_name.as_str().to_string(),
                    decoder: child,
                    cardinality: rel.cardinality,
                });
            } else if rel.cardinality == Cardinality::One {
                let Some(target_ent) = cgs.get_entity(rel.target_resource.as_str()) else {
                    continue;
                };
                let rel_path = PathExpr::new(vec![PathSegment::Key {
                    name: rel_name.as_str().to_string(),
                }]);
                let id_path = PathExpr::new(vec![PathSegment::Key {
                    name: target_ent.id_field.as_str().to_string(),
                }]);
                let child = EntityDecoder::new(rel.target_resource.as_str(), rel_path)
                    .with_id_field(target_ent.id_field.clone())
                    .with_id_path(id_path);
                relation_decoders.push(RelationDecoder {
                    relation: rel_name.as_str().to_string(),
                    decoder: child,
                    cardinality: rel.cardinality,
                });
            }
        }
    }

    let mut decoder = EntityDecoder::new(entity_type, source)
        .with_fields(field_decoders)
        .with_relations(relation_decoders)
        .with_identity_ambient(ambient);
    if let Some(entity) = cgs.get_entity(entity_type) {
        let key_vars: Vec<String> = entity
            .key_vars
            .iter()
            .map(|k| k.as_str().to_string())
            .collect();
        decoder = decoder
            .with_id_field(entity.id_field.clone())
            .with_key_vars(key_vars);
        if let Some(parts) = entity.id_from.as_ref().filter(|p| !p.is_empty()) {
            let segments: Vec<PathSegment> = parts
                .iter()
                .cloned()
                .map(|name| PathSegment::Key { name })
                .collect();
            decoder = decoder.with_id_path(PathExpr::new(segments));
        }
        if let Some(rid) = request_identity {
            decoder = decoder.with_request_identity_override(rid);
        }
    }
    decoder
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::loader::load_schema_dir;

    fn assert_embed_decoders_single_hop(decoder: &EntityDecoder) {
        for rel in &decoder.relations {
            assert!(
                rel.decoder.relations.is_empty(),
                "relation `{}` on `{}` must use a single-hop embed decoder",
                rel.relation,
                decoder.entity
            );
        }
    }

    #[test]
    fn runtime_from_parent_get_embed_decoders_are_single_hop() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("langmatrix");
        let decoder = create_entity_decoder_for_capability(
            "LangItem",
            &cgs,
            Some("langitem_get"),
            None,
            Some("i1"),
            None,
        );
        assert_embed_decoders_single_hop(&decoder);
    }

    #[test]
    fn pokeapi_get_embed_decoders_are_single_hop() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        let cgs = load_schema_dir(&dir).expect("pokeapi");
        let decoder = create_entity_decoder_for_capability(
            "Pokemon",
            &cgs,
            Some("pokemon_get"),
            None,
            None,
            None,
        );
        assert_embed_decoders_single_hop(&decoder);
        let types_rel = decoder
            .relations
            .iter()
            .find(|r| r.relation == "types")
            .expect("types relation decoder");
        assert_eq!(types_rel.decoder.entity, "Type");
    }

    #[test]
    fn pokeapi_type_and_ability_get_embed_decoders_are_single_hop() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/pokeapi");
        let cgs = load_schema_dir(&dir).expect("pokeapi");
        for (entity, cap) in [("Type", "type_get"), ("Ability", "ability_get")] {
            let decoder = create_entity_decoder_for_capability(
                entity,
                &cgs,
                Some(cap),
                None,
                None,
                None,
            );
            assert_embed_decoders_single_hop(&decoder);
            let pokemon_rel = decoder
                .relations
                .iter()
                .find(|r| r.relation == "pokemon")
                .expect("pokemon relation decoder");
            assert_eq!(pokemon_rel.decoder.entity, "Pokemon");
            assert!(
                pokemon_rel.decoder.relations.is_empty(),
                "{entity}.pokemon embed must be single-hop"
            );
        }
    }
}
