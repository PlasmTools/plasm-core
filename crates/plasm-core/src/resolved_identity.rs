//! Scalar + binding identity slots derived from a [`Ref`].
//!
//! Path env projection lives in [`crate::path_env`] — this module only materializes
//! wire slots from Lit/Binding keys ([`IdentitySlot`] cutover).

use crate::expr::{EntityKey, IdentitySlot, Ref};
use crate::schema::EntityDef;
use crate::Value;

/// How entity schema participates in identity slot materialization.
#[derive(Debug, Clone, Copy)]
pub enum IdentityProjectionCtx<'a> {
    /// Bind only legacy `"id"` (and compound parts) — tests / EVM edges without entity schema.
    LegacyIdOnly,
    /// Also bind `id_field` / `key_vars` for simple keys.
    Entity(&'a EntityDef),
}

/// Scalar + binding slots derived from a [`Ref`] for template env binding.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResolvedIdentity {
    /// Wire / CML variable name → Lit string or Binding [`Value::PlasmInputRef`].
    pub slots: std::collections::BTreeMap<String, Value>,
}

impl ResolvedIdentity {
    /// Build identity slots for template env population.
    ///
    /// Always includes legacy `id` = primary Lit (when present). With
    /// [`IdentityProjectionCtx::Entity`], also binds `id_field` and each `key_vars`
    /// entry for simple Lit keys; compound keys copy all Lit/Binding parts.
    /// Empty Lit primary (pathless nullary) invents nothing.
    pub fn from_ref(reference: &Ref, ctx: IdentityProjectionCtx<'_>) -> Self {
        let mut slots = std::collections::BTreeMap::new();

        if reference.is_pathless_nullary() {
            if let EntityKey::Compound(parts) = &reference.key {
                for (k, slot) in parts {
                    if let Some(v) = slot_to_value(slot) {
                        slots.insert(k.clone(), v);
                    }
                }
            }
            return Self { slots };
        }

        match &reference.key {
            EntityKey::Compound(parts) => {
                for (k, slot) in parts {
                    if let Some(v) = slot_to_value(slot) {
                        slots.insert(k.clone(), v);
                    }
                }
                if let Some(primary) = primary_lit_value(reference) {
                    slots.insert("id".to_string(), primary);
                }
            }
            EntityKey::Simple(slot) => {
                if let Some(v) = slot_to_value(slot) {
                    slots.insert("id".to_string(), v.clone());
                    if let IdentityProjectionCtx::Entity(e) = ctx {
                        slots.insert(e.id_field.to_string(), v.clone());
                        for kv in &e.key_vars {
                            if kv.as_str() != e.id_field.as_str() {
                                slots.insert(kv.to_string(), v.clone());
                            }
                        }
                    }
                }
            }
        }

        Self { slots }
    }

    /// Lookup a Lit string slot by template variable name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.slots.get(name).and_then(|v| match v {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        })
    }

    pub fn get_value(&self, name: &str) -> Option<&Value> {
        self.slots.get(name)
    }
}

fn slot_to_value(slot: &IdentitySlot) -> Option<Value> {
    match slot {
        IdentitySlot::Lit(id) if id.as_str().is_empty() => None,
        IdentitySlot::Lit(id) => Some(Value::String(id.to_string())),
        IdentitySlot::Binding(r) => Some(Value::PlasmInputRef(r.clone())),
    }
}

fn primary_lit_value(reference: &Ref) -> Option<Value> {
    match &reference.key {
        EntityKey::Simple(IdentitySlot::Lit(id)) if !id.as_str().is_empty() => {
            Some(Value::String(id.to_string()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{EntityFieldName, EntityName};
    use indexmap::IndexMap as Im;

    fn team_entity() -> EntityDef {
        EntityDef {
            name: EntityName::from("Team"),
            description: String::new(),
            id_field: EntityFieldName::from("key"),
            id_format: None,
            id_from: None,
            fields: Im::new(),
            relations: Im::new(),
            expression_aliases: vec![],
            implicit_request_identity: false,
            key_vars: vec![],
            abstract_entity: false,
            domain_projection_examples: true,
            primary_read: None,
            primary_query: None,
            primary_search: None,
            discovery: None,
        }
    }

    #[test]
    fn simple_ref_binds_id_field_name() {
        let ent = team_entity();
        let reference = Ref::new("Team", "EVA");
        let identity = ResolvedIdentity::from_ref(&reference, IdentityProjectionCtx::Entity(&ent));
        assert_eq!(identity.get("id"), Some("EVA"));
        assert_eq!(identity.get("key"), Some("EVA"));
    }

    #[test]
    fn compound_ref_preserves_parts() {
        let reference = Ref::compound(
            "Ticket",
            std::collections::BTreeMap::from([
                ("owner".into(), "o".into()),
                ("repo".into(), "r".into()),
                ("n".into(), "9".into()),
            ]),
        );
        let identity = ResolvedIdentity::from_ref(&reference, IdentityProjectionCtx::LegacyIdOnly);
        assert_eq!(identity.get("owner"), Some("o"));
        assert_eq!(identity.get("repo"), Some("r"));
        assert_eq!(identity.get("n"), Some("9"));
    }

    #[test]
    fn binding_slot_projects_as_plasm_input_ref() {
        let reference =
            Ref::simple_binding("Pet", crate::PlasmInputRef::node_output("pikachu", vec![]));
        let identity = ResolvedIdentity::from_ref(&reference, IdentityProjectionCtx::LegacyIdOnly);
        assert!(matches!(
            identity.get_value("id"),
            Some(Value::PlasmInputRef(_))
        ));
    }
}
