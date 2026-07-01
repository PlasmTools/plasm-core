//! Teaching-table tokens with CGS-backed payloads and a per-session numeric [`Symbol`].
//!
//! Carry [`TeachingTerm`] in the parser and prompt pipeline; format to `e1` / `m2` / `p3` / `r4` only at
//! serialization boundaries via [`Display`] (or explicit [`std::fmt::Write`]).

use crate::identity::{CapabilityName, CapabilityParamName, EntityFieldName, EntityName, PathMethodSegment, RelationName};
use crate::schema::{capability_path_method_segment, CapabilitySchema, CGS};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Session-local index into the corresponding `e` / `m` / `p` table for this [`SymbolMap`] build.
/// The **kind** (entity vs method vs parameter) is implied by the enclosing [`TeachingTerm`] variant,
/// not by this value.
///
/// [`Display`] on `Symbol` is **digits only** (1-based index). For full `e1` / `m2` / `p3` text, use [`Display`] on [`TeachingTerm`].
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
pub struct Symbol(pub u32);

impl Symbol {
    #[inline]
    pub const fn from_zero_based(i: u32) -> Self {
        Self(i)
    }

    /// `e1` → `0`, `m12` → `11` — digits must be non-empty.
    #[inline]
    pub fn parse_index(sym: &str, prefix: char) -> Option<Self> {
        let rest = sym.strip_prefix(prefix)?;
        if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let n: u32 = rest.parse().ok()?;
        n.checked_sub(1).map(Self)
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.saturating_add(1))
    }
}

/// CGS entity referent (type name).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct EntityRef {
    pub name: EntityName,
}

/// One capability on an entity domain: canonical capability name + path segment for dispatch.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct MethodRef {
    pub domain: EntityName,
    pub capability: CapabilityName,
    pub path_segment: PathMethodSegment,
}

/// Which CGS table a parameter name refers to (disambiguates the flat `p#` namespace).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum ParameterSlot {
    EntityField {
        entity: EntityName,
        field: EntityFieldName,
    },
    Relation {
        entity: EntityName,
        name: RelationName,
    },
    CapabilityInput {
        domain: EntityName,
        capability: CapabilityName,
        param: CapabilityParamName,
    },
}

/// Teaching-table token: CGS payload + session [`Symbol`] (tuple per variant).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum TeachingTerm {
    Entity(EntityRef, Symbol),
    Method(MethodRef, Symbol),
    Parameter(ParameterSlot, Symbol),
}

impl TeachingTerm {
    #[inline]
    pub fn symbol(&self) -> Symbol {
        match self {
            TeachingTerm::Entity(_, s)
            | TeachingTerm::Method(_, s)
            | TeachingTerm::Parameter(_, s) => *s,
        }
    }
}

impl fmt::Display for TeachingTerm {
    /// Symbolic form: `e1`, `m2`, `p3`, `r4` (1-based, matching [`crate::symbol_tuning::SymbolMap`]).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (prefix, sym) = match self {
            TeachingTerm::Entity(_, s) => ('e', s),
            TeachingTerm::Method(_, s) => ('m', s),
            TeachingTerm::Parameter(ParameterSlot::Relation { .. }, s) => ('r', s),
            TeachingTerm::Parameter(_, s) => ('p', s),
        };
        write!(f, "{prefix}{}", sym.0.saturating_add(1))
    }
}

/// Resolve [`MethodRef`] from CGS given domain + kebab path segment.
pub fn method_ref_for_domain_segment(cgs: &CGS, domain: &str, kebab: &str) -> Option<MethodRef> {
    let cap = cgs.capabilities.values().find(|c| {
        c.domain.as_str() == domain && capability_path_method_segment(c).as_str() == kebab
    })?;
    Some(MethodRef {
        domain: cap.domain.clone(),
        capability: cap.name.clone(),
        path_segment: capability_path_method_segment(cap),
    })
}

/// Resolve [`MethodRef`] from CGS given domain + capability wire name.
pub fn method_ref_for_capability(cgs: &CGS, domain: &str, capability: &str) -> Option<MethodRef> {
    let cap = cgs
        .get_capability(capability)
        .filter(|c| c.domain.as_str() == domain)?;
    Some(MethodRef {
        domain: cap.domain.clone(),
        capability: cap.name.clone(),
        path_segment: capability_path_method_segment(cap),
    })
}

/// Classify a canonical identifier name into a [`ParameterSlot`] (same precedence as gloss: cap inputs, then fields, then relations).
pub fn resolve_parameter_slot(
    cgs: &CGS,
    full_entities: &[&str],
    name: &str,
) -> Option<ParameterSlot> {
    let full_set: std::collections::HashSet<&str> = full_entities.iter().copied().collect();
    let mut caps: Vec<&CapabilitySchema> = cgs.capabilities.values().collect();
    caps.sort_by(|a, b| a.name.as_str().cmp(b.name.as_str()));

    for cap in caps {
        if !full_set.contains(cap.domain.as_str()) {
            continue;
        }
        let Some(is) = &cap.input_schema else {
            continue;
        };
        let crate::schema::InputType::Object { fields, .. } = &is.input_type else {
            continue;
        };
        for field in fields {
            if field.name == name {
                return Some(ParameterSlot::CapabilityInput {
                    domain: cap.domain.clone(),
                    capability: cap.name.clone(),
                    param: CapabilityParamName::from(name),
                });
            }
        }
    }

    for e in full_entities {
        let ent = cgs.get_entity(e)?;
        if ent.fields.contains_key(name) {
            return Some(ParameterSlot::EntityField {
                entity: EntityName::new(*e),
                field: EntityFieldName::from(name),
            });
        }
    }

    for e in full_entities {
        let ent = cgs.get_entity(e)?;
        if ent.relations.contains_key(name) {
            return Some(ParameterSlot::Relation {
                entity: EntityName::new(*e),
                name: RelationName::from(name),
            });
        }
    }

    None
}
