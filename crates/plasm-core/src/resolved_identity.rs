//! Map entity [`Ref`] identity slots to CML / HTTP template variable names.
//!
//! Used by the runtime when populating get/delete/invoke CML environments so
//! `id_field` names other than `id` (e.g. Linear `Team.key`) receive the primary
//! reference value without catalog-specific mapping renames.

use std::collections::BTreeMap;

use crate::expr::{EntityKey, Ref};
use crate::schema::EntityDef;

/// Scalar slots derived from a [`Ref`] for template env binding.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedIdentity {
    /// Wire / CML variable name → string value.
    pub slots: BTreeMap<String, String>,
}

impl ResolvedIdentity {
    /// Build identity slots for template env population.
    ///
    /// Always includes legacy `id` = [`Ref::primary_slot_str`]. When `ent` is
    /// provided, also binds `ent.id_field` and each `ent.key_vars` entry for
    /// simple keys; compound keys copy all parts.
    pub fn from_ref(reference: &Ref, ent: Option<&EntityDef>) -> Self {
        let primary = reference.primary_slot_str();
        let mut slots = BTreeMap::new();

        // Pathless nullary Get uses an empty primary slot — do not invent wire bindings
        // (`phone_number=0` / empty-string `exists` traps).
        if primary.is_empty() {
            if let EntityKey::Compound(parts) = &reference.key {
                for (k, v) in parts {
                    if !v.is_empty() {
                        slots.insert(k.clone(), v.clone());
                    }
                }
            }
            return Self { slots };
        }

        slots.insert("id".to_string(), primary.clone());

        match &reference.key {
            EntityKey::Compound(parts) => {
                for (k, v) in parts {
                    if !v.is_empty() {
                        slots.insert(k.clone(), v.clone());
                    }
                }
            }
            EntityKey::Simple(id) => {
                let id_str = id.to_string();
                if let Some(e) = ent {
                    slots.insert(e.id_field.to_string(), id_str.clone());
                    for kv in &e.key_vars {
                        if kv.as_str() != e.id_field.as_str() {
                            slots.insert(kv.to_string(), id_str.clone());
                        }
                    }
                }
            }
        }

        Self { slots }
    }

    /// Lookup a slot by template variable name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.slots.get(name).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    use crate::identity::{EntityFieldName, EntityName};
    use crate::schema::EntityDef;

    fn team_entity() -> EntityDef {
        EntityDef {
            name: EntityName::from("Team"),
            description: String::new(),
            id_field: EntityFieldName::from("key"),
            id_format: None,
            id_from: None,
            fields: IndexMap::new(),
            relations: IndexMap::new(),
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
        let identity = ResolvedIdentity::from_ref(&reference, Some(&ent));
        assert_eq!(identity.get("id"), Some("EVA"));
        assert_eq!(identity.get("key"), Some("EVA"));
    }

    #[test]
    fn compound_ref_preserves_parts() {
        let reference = Ref::compound(
            "Ticket",
            BTreeMap::from([
                ("owner".into(), "o".into()),
                ("repo".into(), "r".into()),
                ("n".into(), "9".into()),
            ]),
        );
        let identity = ResolvedIdentity::from_ref(&reference, None);
        assert_eq!(identity.get("owner"), Some("o"));
        assert_eq!(identity.get("repo"), Some("r"));
        assert_eq!(identity.get("n"), Some("9"));
    }
}
