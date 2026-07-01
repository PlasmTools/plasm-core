//! Typed map keys and opaque teaching symbols for [`super::SymbolTables`].

use crate::identity::{
    CapabilityName, CapabilityParamName, EntityFieldName, EntityName, PathMethodSegment,
    RegistryEntryId, RelationName,
};
use crate::teaching_term::Symbol;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

macro_rules! opaque_sym {
    ($(#[$meta:meta])* $name:ident, $prefix_char:literal) => {
        $(#[$meta])*
        #[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Symbol);

        impl $name {
            pub const PREFIX: char = $prefix_char;

            #[inline]
            pub fn from_zero_based(i: u32) -> Self {
                Self(Symbol::from_zero_based(i))
            }

            #[inline]
            pub fn index(&self) -> Symbol {
                self.0
            }

            #[inline]
            pub fn parse(s: &str) -> Option<Self> {
                Symbol::parse_index(s.trim(), Self::PREFIX).map(Self)
            }

            #[inline]
            pub fn as_wire(&self) -> String {
                format!("{}{}", Self::PREFIX, self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", Self::PREFIX, self.0)
            }
        }

        impl FromStr for $name {
            type Err = ();

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::parse(s).ok_or(())
            }
        }
    };
}

opaque_sym!(/// Session entity token (`e1`, `e2`, …).
    OpaqueESym, 'e');
opaque_sym!(/// Session method token (`m1`, `m2`, …).
    OpaqueMSym, 'm');
opaque_sym!(/// Session parameter / field token (`p1`, `p2`, …).
    OpaquePSym, 'p');
opaque_sym!(/// Session relation token (`r1`, `r2`, …).
    OpaqueRSym, 'r');
opaque_sym!(/// Session value-domain token (`v1`, `v2`, …).
    OpaqueVSym, 'v');

impl OpaqueESym {
    #[inline]
    pub fn is_token(s: &str) -> bool {
        Self::parse(s).is_some()
    }
}

impl OpaqueMSym {
    #[inline]
    pub fn is_token(s: &str) -> bool {
        Self::parse(s).is_some()
    }
}

impl OpaquePSym {
    #[inline]
    pub fn is_token(s: &str) -> bool {
        Self::parse(s).is_some()
    }
}

impl OpaqueRSym {
    #[inline]
    pub fn is_token(s: &str) -> bool {
        Self::parse(s).is_some()
    }
}

impl OpaqueVSym {
    #[inline]
    pub fn is_token(s: &str) -> bool {
        Self::parse(s).is_some()
    }
}

/// `(registry entry_id, CGS entity name)` — opaque `e#` forward key and federation dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct QualifiedEntityKey {
    #[serde(alias = "catalog_entry_id")]
    pub entry_id: RegistryEntryId,
    pub entity: EntityName,
}

impl QualifiedEntityKey {
    pub fn new(entry_id: impl Into<RegistryEntryId>, entity: impl Into<EntityName>) -> Self {
        Self {
            entry_id: entry_id.into(),
            entity: entity.into(),
        }
    }

    /// Registry row id (plan JSON and HTTP use the same wire name as `entry_id`).
    #[inline]
    pub fn entry_id(&self) -> &str {
        self.entry_id.as_str()
    }
}

/// `(registry entry_id, domain entity, capability wire)` — opaque `m#` forward key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MethodKey {
    pub entry_id: RegistryEntryId,
    pub domain: EntityName,
    pub capability: CapabilityName,
}

impl MethodKey {
    pub fn new(
        entry_id: impl Into<RegistryEntryId>,
        domain: impl Into<EntityName>,
        capability: impl Into<CapabilityName>,
    ) -> Self {
        Self {
            entry_id: entry_id.into(),
            domain: domain.into(),
            capability: capability.into(),
        }
    }
}

/// Path segment index for `method_sym_for(..., "create")` without scanning.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MethodSegmentKey {
    pub entry_id: RegistryEntryId,
    pub domain: EntityName,
    pub segment: PathMethodSegment,
}

/// `(registry entry_id, entity, field wire)` — opaque `p#` forward key for entity fields.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityFieldKey {
    pub entry_id: RegistryEntryId,
    pub entity: EntityName,
    pub field: EntityFieldName,
}

impl EntityFieldKey {
    pub fn new(
        entry_id: impl Into<RegistryEntryId>,
        entity: impl Into<EntityName>,
        field: impl Into<EntityFieldName>,
    ) -> Self {
        Self {
            entry_id: entry_id.into(),
            entity: entity.into(),
            field: field.into(),
        }
    }
}

/// `(registry entry_id, domain, capability, param wire)` — opaque `p#` forward key for cap inputs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CapParamKey {
    pub entry_id: RegistryEntryId,
    pub domain: EntityName,
    pub capability: CapabilityName,
    pub param: CapabilityParamName,
}

impl CapParamKey {
    pub fn new(
        entry_id: impl Into<RegistryEntryId>,
        domain: impl Into<EntityName>,
        capability: impl Into<CapabilityName>,
        param: impl Into<CapabilityParamName>,
    ) -> Self {
        Self {
            entry_id: entry_id.into(),
            domain: domain.into(),
            capability: capability.into(),
            param: param.into(),
        }
    }
}

/// `(registry entry_id, entity, relation wire)` — opaque `r#` forward key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelationKey {
    pub entry_id: RegistryEntryId,
    pub entity: EntityName,
    pub relation: RelationName,
}

impl RelationKey {
    pub fn new(
        entry_id: impl Into<RegistryEntryId>,
        entity: impl Into<EntityName>,
        relation: impl Into<RelationName>,
    ) -> Self {
        Self {
            entry_id: entry_id.into(),
            entity: entity.into(),
            relation: relation.into(),
        }
    }
}

/// Catalog provenance for symbol resolve/render — no empty-string sentinels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogScope<'a> {
    /// Session reverse maps only (`sym_to_slot`, `sym_to_method`, …).
    SessionReverse,
    /// Registry row required for qualified forward-map keys.
    Qualified(&'a str),
}

impl<'a> CatalogScope<'a> {
    #[inline]
    pub fn qualified(entry_id: &'a str) -> Self {
        if entry_id.is_empty() {
            Self::SessionReverse
        } else {
            Self::Qualified(entry_id)
        }
    }

    #[inline]
    pub fn from_forward_map_key(entry_id: &'a str) -> Self {
        Self::qualified(entry_id)
    }

    #[inline]
    pub fn from_session_catalog(entry: crate::catalog_id::SessionCatalogEntryId<'a>) -> Self {
        match entry.as_str() {
            Some(id) => Self::Qualified(id),
            None => Self::SessionReverse,
        }
    }

    #[inline]
    pub fn entry_id(self) -> Option<&'a str> {
        match self {
            Self::SessionReverse => None,
            Self::Qualified("") => None,
            Self::Qualified(id) => Some(id),
        }
    }

    #[inline]
    pub fn matches_entry(self, entry_id: &str) -> bool {
        match self {
            Self::SessionReverse => true,
            Self::Qualified(id) => !id.is_empty() && id == entry_id,
        }
    }
}
