//! Compiled catalog interchange (CBOR IL) for platform-independent distribution.
//!
//! Artifacts: `<entry_id>.v<version>.<hash12>.cgs.cbor` + sibling `.manifest.json`.

use crate::schema::CGS;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Current compiled-catalog wire format version (manifest + CBOR body).
pub const PLASM_CATALOG_FORMAT_VERSION: u32 = 1;

/// Sidecar manifest for a compiled catalog artifact (JSON on disk).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogManifest {
    pub format_version: u32,
    pub entry_id: String,
    pub version: u64,
    /// Hex SHA-256 of canonical JSON for the embedded CGS ([`CGS::catalog_cgs_hash_hex`]).
    pub cgs_hash: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Basename of the CBOR artifact in the same directory (e.g. `github.v3.a1b2c3d4e5f6.cgs.cbor`).
    pub cgs_cbor: String,
}

impl CatalogManifest {
    pub fn validate_format(&self) -> Result<(), String> {
        if self.format_version != PLASM_CATALOG_FORMAT_VERSION {
            return Err(format!(
                "unsupported catalog format_version {} (expected {PLASM_CATALOG_FORMAT_VERSION})",
                self.format_version
            ));
        }
        if self.entry_id.is_empty() {
            return Err("catalog manifest entry_id must be non-empty".into());
        }
        if self.version == 0 {
            return Err(format!(
                "catalog manifest version must be > 0 for `{}`",
                self.entry_id
            ));
        }
        if self.cgs_hash.len() != 64 || !self.cgs_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!(
                "catalog manifest cgs_hash must be 64 hex chars for `{}`",
                self.entry_id
            ));
        }
        if self.cgs_cbor.is_empty() {
            return Err(format!(
                "catalog manifest cgs_cbor must be non-empty for `{}`",
                self.entry_id
            ));
        }
        Ok(())
    }
}

/// Serialize a validated CGS to compiled CBOR IL bytes.
pub fn cgs_to_catalog_il_cbor(cgs: &CGS) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(cgs, &mut bytes)
        .map_err(|e| format!("CGS CBOR encode failed: {e}"))?;
    Ok(bytes)
}

/// Decode compiled CBOR IL bytes into a CGS and run full validation.
pub fn load_catalog_il_cbor(bytes: &[u8]) -> Result<CGS, String> {
    let cgs: CGS =
        ciborium::de::from_reader(bytes).map_err(|e| format!("CGS CBOR decode failed: {e}"))?;
    cgs.validate()
        .map_err(|e| format!("CGS validation failed after CBOR decode: {e}"))?;
    Ok(cgs)
}

/// Decode CBOR IL and verify digest matches the manifest `cgs_hash`.
pub fn load_catalog_il_cbor_verified(bytes: &[u8], expected_hash: &str) -> Result<CGS, String> {
    let cgs = load_catalog_il_cbor(bytes)?;
    let actual = cgs.catalog_cgs_hash_hex();
    if actual != expected_hash {
        return Err(format!(
            "catalog cgs_hash mismatch: manifest {expected_hash}, decoded CGS {actual}"
        ));
    }
    Ok(cgs)
}

fn stale_catalog_hint(detail: &str) -> &'static str {
    if detail.contains("value_ref") || detail.contains("input_type") {
        " Remove stale catalog artifacts under the catalog dir or rebuild: `cargo run -p plasm --bin plasm-pack-catalogs -- --workspace . --apis-root apis --output-dir target/plasm-catalogs --force`"
    } else {
        ""
    }
}

/// Read and parse a catalog manifest JSON file, validating wire-format fields.
pub fn read_catalog_manifest(path: &Path) -> Result<CatalogManifest, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("read manifest {}: {e}", path.display()))?;
    let manifest: CatalogManifest = serde_json::from_str(&raw)
        .map_err(|e| format!("parse manifest JSON {}: {e}", path.display()))?;
    manifest
        .validate_format()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(manifest)
}

/// Load CGS from a manifest sidecar and its CBOR artifact in `dir`.
pub fn load_catalog_artifact(dir: &Path, manifest: &CatalogManifest) -> Result<CGS, String> {
    let cbor_path = dir.join(&manifest.cgs_cbor);
    if !cbor_path.is_file() {
        return Err(format!(
            "missing CBOR artifact `{}` for entry `{}`",
            manifest.cgs_cbor, manifest.entry_id
        ));
    }
    let bytes =
        std::fs::read(&cbor_path).map_err(|e| format!("read CBOR {}: {e}", cbor_path.display()))?;
    let cgs = match load_catalog_il_cbor_verified(&bytes, &manifest.cgs_hash) {
        Ok(cgs) => cgs,
        Err(e) => {
            let detail = e.to_string();
            return Err(format!(
                "{}: decode CBOR IL: {}{}",
                manifest.entry_id,
                detail,
                stale_catalog_hint(&detail)
            ));
        }
    };
    if cgs.version != manifest.version {
        return Err(format!(
            "version mismatch for entry `{}`: manifest {}, CGS {}",
            manifest.entry_id, manifest.version, cgs.version
        ));
    }
    if cgs.entry_id.as_deref() != Some(manifest.entry_id.as_str()) {
        return Err(format!(
            "entry_id mismatch for `{}`: manifest vs CGS {:?}",
            manifest.entry_id, cgs.entry_id
        ));
    }
    Ok(cgs)
}

/// True when `path` is a compiled-catalog manifest sidecar (`*.manifest.json`).
pub fn is_catalog_manifest_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "json")
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".manifest.json"))
}

/// Filename stem for a packed catalog: `<entry_id>.v<version>.<hash12>`.
pub fn catalog_artifact_stem(entry_id: &str, version: u64, cgs_hash_hex: &str) -> String {
    let short_hash = cgs_hash_hex.chars().take(12).collect::<String>();
    format!("{entry_id}.v{version}.{short_hash}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;

    #[test]
    fn catalog_il_cbor_round_trip_preserves_cgs_hash() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/pokeapi_mini");
        let cgs = load_schema_dir(&dir).expect("load pokeapi_mini fixture");
        let hash_before = cgs.catalog_cgs_hash_hex();
        let bytes = cgs_to_catalog_il_cbor(&cgs).expect("encode");
        assert!(!bytes.is_empty(), "CBOR payload must be non-empty");
        let decoded = load_catalog_il_cbor_verified(&bytes, &hash_before).expect("decode+verify");
        assert_eq!(decoded.catalog_cgs_hash_hex(), hash_before);
    }

    #[test]
    fn catalog_manifest_validate_rejects_bad_version() {
        let m = CatalogManifest {
            format_version: PLASM_CATALOG_FORMAT_VERSION,
            entry_id: "test".into(),
            version: 0,
            cgs_hash: "a".repeat(64),
            label: String::new(),
            tags: vec![],
            cgs_cbor: "x.cgs.cbor".into(),
        };
        assert!(m.validate_format().is_err());
    }
}
