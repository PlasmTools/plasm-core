//! Opaque teaching symbol resolution for prompt render.

use crate::schema::CapabilitySchema;
use crate::symbol_tuning::{CapParamTeachingSurface, SymbolMap, TeachingExposureSession};
use indexmap::IndexMap;

/// Owning `entry_id` for an exposed entity wire name when it appears under exactly one catalog row.
#[inline]
pub(crate) fn catalog_entry_id_for_exposed_entity<'a>(
    qualified: &IndexMap<(&'a str, &'a str), ()>,
    entity: &str,
) -> Option<&'a str> {
    let mut matches: Vec<_> = qualified.keys().filter(|(_, e)| *e == entity).collect();
    match matches.len() {
        1 => Some(matches.pop().expect("len 1").0),
        _ => None,
    }
}

#[inline]
pub(crate) fn exposure_qualified_catalog_ids(
    exposure: &TeachingExposureSession,
) -> IndexMap<(&str, &str), ()> {
    exposure
        .entities
        .iter()
        .zip(exposure.entity_catalog_entry_ids.iter())
        .map(|(entity, entry_id)| ((entry_id.as_str(), entity.as_str()), ()))
        .collect()
}

#[inline]
pub(crate) fn ent_sym(m: Option<&SymbolMap>, catalog_entry_id: &str, c: &str) -> String {
    m.map(|x| x.entity_sym_for(catalog_entry_id, c))
        .unwrap_or_else(|| c.to_string())
}

#[inline]
pub(crate) fn id_sym_entity(
    m: Option<&SymbolMap>,
    catalog_entry_id: &str,
    entity: &str,
    field: &str,
) -> String {
    m.map(|x| x.ident_sym_entity_field_for(catalog_entry_id, entity, field))
        .unwrap_or_else(|| field.to_string())
}

#[inline]
pub(crate) fn id_sym_cap(
    m: Option<&SymbolMap>,
    catalog_entry_id: &str,
    cap: &CapabilitySchema,
    param: &str,
    surface: CapParamTeachingSurface,
) -> String {
    m.map(|x| {
        x.cap_param_teaching_token(
            catalog_entry_id,
            cap.domain.as_str(),
            cap.name.as_str(),
            param,
            surface,
        )
    })
    .unwrap_or_else(|| param.to_string())
}

#[inline]
pub(crate) fn id_sym_rel(
    m: Option<&SymbolMap>,
    catalog_entry_id: &str,
    entity: &str,
    rel: &str,
) -> String {
    m.map(|x| x.ident_sym_relation_for(catalog_entry_id, entity, rel))
        .unwrap_or_else(|| rel.to_string())
}

#[inline]
pub(crate) fn met_sym(
    m: Option<&SymbolMap>,
    catalog_entry_id: &str,
    entity: &str,
    cap: &CapabilitySchema,
) -> String {
    m.map(|x| x.method_sym_for(catalog_entry_id, entity, cap.name.as_str()))
        .unwrap_or_else(|| crate::schema::capability_method_label_kebab(cap))
}
