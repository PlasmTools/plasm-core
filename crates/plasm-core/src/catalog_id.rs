//! Session catalog ownership types — labelled unions instead of empty-string sentinels.

use crate::identity::RegistryEntryId;
use serde::{Deserialize, Deserializer, Serializer};

/// Empty registry `entry_id` strings are invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyRegistryEntryId;

impl std::fmt::Display for EmptyRegistryEntryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("registry entry_id must be non-empty")
    }
}

impl std::error::Error for EmptyRegistryEntryId {}

impl RegistryEntryId {
    /// Construct only when `s` is non-empty after trim.
    pub fn try_new(s: impl AsRef<str>) -> Result<Self, EmptyRegistryEntryId> {
        let s = s.as_ref();
        if s.is_empty() {
            return Err(EmptyRegistryEntryId);
        }
        Ok(Self::from(s))
    }
}

/// Catalog row provenance for parse layers and fixture single-graph sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionCatalogEntryId<'a> {
    /// YAML fixtures / pre-exposure single CGS — reverse symbol maps only.
    Unset,
    /// Owning registry row (non-empty).
    Known(&'a str),
}

impl<'a> SessionCatalogEntryId<'a> {
    #[inline]
    pub const fn unset() -> Self {
        Self::Unset
    }

    #[inline]
    pub fn known(entry_id: &'a str) -> Result<Self, EmptyRegistryEntryId> {
        if entry_id.is_empty() {
            return Err(EmptyRegistryEntryId);
        }
        Ok(Self::Known(entry_id))
    }

    /// Transition helper — maps legacy empty strings to [`Unset`].
    #[inline]
    pub fn from_entry_str(entry_id: &'a str) -> Self {
        if entry_id.is_empty() {
            Self::Unset
        } else {
            Self::Known(entry_id)
        }
    }

    #[inline]
    pub fn as_str(self) -> Option<&'a str> {
        match self {
            Self::Unset => None,
            Self::Known(id) => Some(id),
        }
    }

    #[inline]
    pub fn is_unset(self) -> bool {
        matches!(self, Self::Unset)
    }
}

/// Optional session catalog stamp on [`crate::Expr`] variants (owned, serde-normalized).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CatalogEntryStamp(pub Option<RegistryEntryId>);

impl CatalogEntryStamp {
    #[inline]
    pub const fn none() -> Self {
        Self(None)
    }

    #[inline]
    pub fn some(entry_id: RegistryEntryId) -> Self {
        Self(Some(entry_id))
    }

    #[inline]
    pub fn as_ref(&self) -> Option<&RegistryEntryId> {
        self.0.as_ref()
    }

    #[inline]
    pub fn as_deref(&self) -> Option<&str> {
        self.0.as_ref().map(|id| id.as_str())
    }

    #[inline]
    pub fn is_none(&self) -> bool {
        self.0.is_none()
    }

    #[inline]
    pub fn stamp_from_session(id: Option<&RegistryEntryId>) -> Self {
        id.map(|entry| Self::some(entry.clone()))
            .unwrap_or_default()
    }

    #[inline]
    pub fn from_opt_str(v: Option<impl AsRef<str>>) -> Self {
        match v {
            None => Self::none(),
            Some(s) if s.as_ref().is_empty() => Self::none(),
            Some(s) => Self::some(RegistryEntryId::from(s.as_ref())),
        }
    }
}

/// Registry row id for symbol tables when the YAML graph has no `entry_id` (empty → [`SessionCatalogEntryId::Unset`]).
#[inline]
pub fn cgs_session_catalog_id(cgs: &crate::CGS) -> SessionCatalogEntryId<'_> {
    SessionCatalogEntryId::from_entry_str(cgs.entry_id.as_deref().unwrap_or(""))
}

/// Wire key for forward symbol maps (`""` when unset — matches exposure assignment).
#[inline]
pub fn cgs_symbol_map_entry_key(cgs: &crate::CGS) -> &str {
    cgs_session_catalog_id(cgs).as_str().unwrap_or("")
}
pub mod catalog_entry_stamp {
    pub use super::{deserialize_catalog_stamp as deserialize, serialize_catalog_stamp as serialize};
}

impl Default for CatalogEntryStamp {
    fn default() -> Self {
        Self::none()
    }
}

impl From<Option<RegistryEntryId>> for CatalogEntryStamp {
    fn from(v: Option<RegistryEntryId>) -> Self {
        Self(v)
    }
}

impl From<CatalogEntryStamp> for Option<RegistryEntryId> {
    fn from(v: CatalogEntryStamp) -> Self {
        v.0
    }
}

/// Serde: `null`, missing, and `""` → `None`; non-empty → [`RegistryEntryId`].
pub fn deserialize_catalog_stamp<'de, D>(deserializer: D) -> Result<CatalogEntryStamp, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        None => Ok(CatalogEntryStamp::none()),
        Some(s) if s.is_empty() => Ok(CatalogEntryStamp::none()),
        Some(s) => RegistryEntryId::try_new(s)
            .map(CatalogEntryStamp::some)
            .map_err(serde::de::Error::custom),
    }
}

pub fn serialize_catalog_stamp<S>(
    stamp: &CatalogEntryStamp,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match &stamp.0 {
        None => serializer.serialize_none(),
        Some(id) => serializer.serialize_str(id.as_str()),
    }
}
