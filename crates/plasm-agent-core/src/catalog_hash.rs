//! Typed catalog digest newtypes — registry-base vs post-overlay effective pins.

use indexmap::IndexMap;
use plasm_core::CGS;
use std::collections::HashMap;

/// Registry YAML digest at open (before tenant/http/overlay patches).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RegistryCatalogHash(String);

/// Post-overlay effective catalog digest used for symbol-ledger pins and reuse keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct EffectiveCatalogHash(String);

impl RegistryCatalogHash {
    pub(crate) fn from_registry_cgs(cgs: &CGS) -> Self {
        Self(cgs.catalog_cgs_hash_hex())
    }

    pub(crate) fn from_hex(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl EffectiveCatalogHash {
    pub(crate) fn from_effective_cgs(cgs: &CGS) -> Self {
        Self(cgs.effective_catalog_cgs_hash_hex())
    }

    pub(crate) fn from_hex(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<EffectiveCatalogHash> for String {
    fn from(v: EffectiveCatalogHash) -> Self {
        v.0
    }
}

impl From<&EffectiveCatalogHash> for String {
    fn from(v: &EffectiveCatalogHash) -> Self {
        v.0.clone()
    }
}

impl From<RegistryCatalogHash> for String {
    fn from(v: RegistryCatalogHash) -> Self {
        v.0
    }
}

pub(crate) fn effective_hash_map_to_strings(
    map: &IndexMap<String, EffectiveCatalogHash>,
) -> HashMap<String, String> {
    map.iter()
        .map(|(k, v)| (k.clone(), v.as_str().to_string()))
        .collect()
}

pub(crate) fn effective_hashes_from_string_map(
    map: &HashMap<String, String>,
) -> IndexMap<String, EffectiveCatalogHash> {
    map.iter()
        .map(|(k, v)| (k.clone(), EffectiveCatalogHash::from_hex(v.clone())))
        .collect()
}

pub(crate) fn registry_hashes_to_strings(
    map: &HashMap<String, RegistryCatalogHash>,
) -> HashMap<String, String> {
    map.iter()
        .map(|(k, v)| (k.clone(), v.as_str().to_string()))
        .collect()
}
