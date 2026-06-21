//! CGS `from_parent_get` embed target decoders (CEP-10: leaf decoders have no nested `.relations`).

use indexmap::IndexMap;

use plasm_core::CGS;

use crate::decoder::{
    EntityDecoder, FieldDecoder, ParentIdentityFieldHint, PathExpr, PathSegment,
};

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
    let parent_hints = parent_identity_field_hints_for_child(parent_ent, target_ent);
    let decoder = EntityDecoder::new(target_ent.name.as_str(), rel_path)
        .with_fields(cf)
        .with_id_field(target_ent.id_field.clone())
        .with_key_vars(child_kv)
        .with_identity_ambient(IndexMap::new())
        .with_parent_identity_field_hints(parent_hints);
    debug_assert!(
        decoder.relations.is_empty(),
        "from_parent_get target decoder must not nest relation embed decoders (CEP-10)"
    );
    decoder
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

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::loader::load_schema_dir;

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
