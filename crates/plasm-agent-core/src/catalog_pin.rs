//! Catalog digest pins: client/server agreement on loaded registry `entry_id` + CGS hash.

use indexmap::IndexMap;
use plasm_core::CgsContext;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

use crate::execute_session::ExecuteSession;

/// Registry `entry_id` + canonical [`plasm_core::schema::CGS::catalog_cgs_hash_hex`] pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPin {
    pub api: String,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CatalogPinError {
    #[error("catalog_pins must be non-empty")]
    EmptyPins,
    #[error("catalog_pins: {field} must be non-empty")]
    EmptyField { field: &'static str },
    #[error("catalog digest mismatch for `{entry_id}`")]
    DigestMismatch { entry_id: String },
    #[error("execute session is missing catalog pin for `{entry_id}`")]
    MissingPin { entry_id: String },
    #[error("catalog `{entry_id}` is pinned but not loaded in execute session")]
    UnloadedPin { entry_id: String },
}

impl ExecuteSession {
    /// Every pinned catalog must match the live execute session; every loaded catalog must be pinned.
    pub fn validate_catalog_pins(&self, pins: &[CatalogPin]) -> Result<(), CatalogPinError> {
        validate_catalog_pins_against_contexts(&self.contexts_by_entry, pins)
    }
}

pub(crate) fn validate_catalog_pins_against_contexts(
    contexts_by_entry: &IndexMap<String, Arc<CgsContext>>,
    pins: &[CatalogPin],
) -> Result<(), CatalogPinError> {
    if pins.is_empty() {
        return Err(CatalogPinError::EmptyPins);
    }
    let mut pinned: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for pin in pins {
        let api = pin.api.trim();
        let digest = pin.digest.trim();
        if api.is_empty() {
            return Err(CatalogPinError::EmptyField { field: "api" });
        }
        if digest.is_empty() {
            return Err(CatalogPinError::EmptyField { field: "digest" });
        }
        pinned.insert(api, digest);
    }
    for (entry_id, ctx) in contexts_by_entry {
        let server_digest = ctx.cgs.catalog_cgs_hash_hex();
        match pinned.get(entry_id.as_str()) {
            Some(client_digest) if *client_digest == server_digest.as_str() => {}
            Some(_) => {
                return Err(CatalogPinError::DigestMismatch {
                    entry_id: entry_id.clone(),
                });
            }
            None => {
                return Err(CatalogPinError::MissingPin {
                    entry_id: entry_id.clone(),
                });
            }
        }
    }
    for pin in pins {
        let api = pin.api.trim();
        if !contexts_by_entry.contains_key(api) {
            return Err(CatalogPinError::UnloadedPin {
                entry_id: api.to_string(),
            });
        }
    }
    Ok(())
}
