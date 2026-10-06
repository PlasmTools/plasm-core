//! Compiled catalog interchange (JSON IL) for platform-independent distribution.
//!
//! Artifacts: `<entry_id>.v<version>.<hash12>.cgs.json` + sibling `.manifest.json`.
//! Wire bytes are serde JSON for the CGS — the same canonical form as [`CGS::catalog_cgs_hash_hex`].

use crate::schema::CGS;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum CatalogIlError {
    #[error("catalog artifact IO failed")]
    Io(#[from] std::io::Error),
    #[error("catalog artifact JSON is invalid")]
    Json(#[from] serde_json::Error),
    #[error("catalog schema validation failed")]
    Schema(#[from] crate::error::SchemaError),
    #[error("catalog format version is unsupported: {actual}")]
    UnsupportedFormat { actual: u32 },
    #[error("catalog set must contain manifests")]
    EmptyCatalogSet,
    #[error("catalog set contains an invalid or duplicate manifest name")]
    InvalidCatalogManifestName,
    #[error("catalog manifest entry identifier is empty")]
    EmptyEntryId,
    #[error("catalog manifest version must be nonzero")]
    ZeroVersion,
    #[error("catalog manifest digest is invalid: {field}")]
    InvalidDigest { field: &'static str },
    #[error("catalog artifact name must be a basename: {field}")]
    InvalidArtifactName { field: &'static str },
    #[error("catalog artifact digest does not match manifest: {field}")]
    DigestMismatch { field: &'static str },
    #[error("catalog embedding profile is unsupported")]
    EmbeddingProfile,
    #[error("catalog embedding profiles disagree")]
    EmbeddingProfileMismatch,
    #[error("catalog format version {actual} is unsupported")]
    ManifestFormat { actual: u32 },
    #[error("catalog artifact `{name}` is missing")]
    MissingArtifact { name: String },
    #[error("catalog version differs from its manifest")]
    VersionMismatch { manifest: u64, catalog: u64 },
    #[error("catalog entry identifier differs from its manifest")]
    EntryIdMismatch,
    #[error(transparent)]
    Discovery(#[from] crate::catalog_discovery::CatalogDiscoveryError),
}

/// Current compiled-catalog wire format version (manifest + JSON body).
pub const PLASM_CATALOG_FORMAT_VERSION: u32 = 3;

/// Filename suffix for compiled catalog body artifacts.
pub const CATALOG_IL_BODY_SUFFIX: &str = ".cgs.json";

/// Atomic publication boundary for the complete packed catalog set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSetManifest {
    pub format_version: u32,
    pub manifests: Vec<String>,
}

pub fn read_catalog_set(dir: &Path) -> Result<Vec<std::path::PathBuf>, CatalogIlError> {
    let bytes = std::fs::read(dir.join("catalog-set.json"))?;
    let set: CatalogSetManifest = serde_json::from_slice(&bytes)?;
    if set.format_version != PLASM_CATALOG_FORMAT_VERSION {
        return Err(CatalogIlError::UnsupportedFormat {
            actual: set.format_version,
        });
    }
    if set.manifests.is_empty() {
        return Err(CatalogIlError::EmptyCatalogSet);
    }
    let mut names = std::collections::BTreeSet::new();
    for name in &set.manifests {
        if Path::new(name).file_name().and_then(|s| s.to_str()) != Some(name)
            || !name.ends_with(".manifest.json")
            || !names.insert(name)
        {
            return Err(CatalogIlError::InvalidCatalogManifestName);
        }
    }
    Ok(set
        .manifests
        .into_iter()
        .map(|name| dir.join(name))
        .collect())
}

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
    /// Basename of the JSON artifact in the same directory (e.g. `github.v3.a1b2c3d4e5f6.cgs.json`).
    pub cgs_json: String,
    /// Basename of the compiled CML request-recipe artifact.
    pub recipes_json: String,
    /// SHA-256 of `recipes_json` bytes.
    pub recipes_hash: String,
    pub discovery_json: String,
    pub discovery_hash: String,
    pub embedding_profile: crate::catalog_discovery::EmbeddingProfile,
}

impl CatalogManifest {
    pub fn validate_format(&self) -> Result<(), CatalogIlError> {
        self.embedding_profile
            .validate()
            .map_err(|_| CatalogIlError::EmbeddingProfile)?;
        if self.format_version != PLASM_CATALOG_FORMAT_VERSION {
            return Err(CatalogIlError::ManifestFormat {
                actual: self.format_version,
            });
        }
        if self.entry_id.is_empty() {
            return Err(CatalogIlError::EmptyEntryId);
        }
        if self.version == 0 {
            return Err(CatalogIlError::ZeroVersion);
        }
        if self.cgs_hash.len() != 64 || !self.cgs_hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(CatalogIlError::InvalidDigest { field: "cgs_hash" });
        }
        if self.cgs_json.is_empty() {
            return Err(CatalogIlError::InvalidArtifactName { field: "cgs_json" });
        }
        for name in [&self.cgs_json, &self.recipes_json, &self.discovery_json] {
            if name.is_empty() || Path::new(name).file_name().and_then(|n| n.to_str()) != Some(name)
            {
                return Err(CatalogIlError::InvalidArtifactName { field: "manifest" });
            }
        }
        if self.recipes_hash.len() != 64
            || !self.recipes_hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(CatalogIlError::InvalidDigest {
                field: "recipes_hash",
            });
        }
        if self.discovery_hash.len() != 64
            || !self.discovery_hash.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(CatalogIlError::InvalidDigest {
                field: "discovery_hash",
            });
        }
        Ok(())
    }
}

/// Serialize a validated CGS to compiled JSON IL bytes.
pub fn cgs_to_catalog_il_bytes(cgs: &CGS) -> Result<Vec<u8>, CatalogIlError> {
    Ok(serde_json::to_vec(cgs)?)
}

/// Decode compiled JSON IL bytes into a CGS and run full validation.
pub fn load_catalog_il_bytes(bytes: &[u8]) -> Result<CGS, CatalogIlError> {
    let span = crate::spans::catalog_load_il(bytes.len());
    let _guard = span.enter();
    let mut cgs: CGS = serde_json::from_slice(bytes)?;
    cgs.stamp_entity_ref_catalogs();
    cgs.validate()?;
    Ok(cgs)
}

/// Verify discovery bytes and every embedded capability before use by a host.
pub fn load_discovery_artifact(
    dir: &Path,
    manifest: &CatalogManifest,
    cgs: &CGS,
) -> Result<crate::catalog_discovery::CatalogDiscoveryArtifact, CatalogIlError> {
    manifest.validate_format()?;
    let bytes = std::fs::read(dir.join(&manifest.discovery_json))?;
    if crate::catalog_discovery::content_hash(&bytes) != manifest.discovery_hash {
        return Err(CatalogIlError::DigestMismatch { field: "discovery" });
    }
    let artifact: crate::catalog_discovery::CatalogDiscoveryArtifact =
        serde_json::from_slice(&bytes)?;
    if artifact.profile != manifest.embedding_profile {
        return Err(CatalogIlError::EmbeddingProfileMismatch);
    }
    artifact.validate(cgs)?;
    Ok(artifact)
}

/// Decode JSON IL and verify digest matches the manifest `cgs_hash`.
pub fn load_catalog_il_verified(bytes: &[u8], expected_hash: &str) -> Result<CGS, CatalogIlError> {
    let cgs = load_catalog_il_bytes(bytes)?;
    let actual = cgs.catalog_cgs_hash_hex();
    if actual != expected_hash {
        return Err(CatalogIlError::DigestMismatch { field: "cgs_hash" });
    }
    Ok(cgs)
}

/// Read and parse a catalog manifest JSON file, validating wire-format fields.
pub fn read_catalog_manifest(path: &Path) -> Result<CatalogManifest, CatalogIlError> {
    let raw = std::fs::read_to_string(path)?;
    let manifest: CatalogManifest = serde_json::from_str(&raw)?;
    manifest.validate_format()?;
    Ok(manifest)
}

/// Load CGS from a manifest sidecar and its JSON artifact in `dir`.
pub fn load_catalog_artifact(
    dir: &Path,
    manifest: &CatalogManifest,
) -> Result<CGS, CatalogIlError> {
    let json_path = dir.join(&manifest.cgs_json);
    if !json_path.is_file() {
        return Err(CatalogIlError::MissingArtifact {
            name: manifest.cgs_json.clone(),
        });
    }
    let bytes = std::fs::read(&json_path)?;
    let cgs = load_catalog_il_verified(&bytes, &manifest.cgs_hash)?;
    if cgs.version != manifest.version {
        return Err(CatalogIlError::VersionMismatch {
            manifest: manifest.version,
            catalog: cgs.version,
        });
    }
    if cgs.entry_id.as_deref() != Some(manifest.entry_id.as_str()) {
        return Err(CatalogIlError::EntryIdMismatch);
    }
    load_discovery_artifact(dir, manifest, &cgs)?;
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

/// Basename for a packed catalog JSON body artifact.
pub fn catalog_il_body_name(entry_id: &str, version: u64, cgs_hash_hex: &str) -> String {
    format!(
        "{}{}",
        catalog_artifact_stem(entry_id, version, cgs_hash_hex),
        CATALOG_IL_BODY_SUFFIX
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader::load_schema_dir;

    #[test]
    fn catalog_il_json_round_trip_preserves_cgs_hash() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/pokeapi_mini");
        let cgs = load_schema_dir(&dir).expect("load pokeapi_mini fixture");
        let hash_before = cgs.catalog_cgs_hash_hex();
        let bytes = cgs_to_catalog_il_bytes(&cgs).expect("encode");
        assert!(!bytes.is_empty(), "JSON payload must be non-empty");
        let decoded = load_catalog_il_verified(&bytes, &hash_before).expect("decode+verify");
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
            cgs_json: "x.cgs.json".into(),
            recipes_json: "x.recipes.json".into(),
            recipes_hash: "c".repeat(64),
            discovery_json: "x.discovery.json".into(),
            discovery_hash: "b".repeat(64),
            embedding_profile: Default::default(),
        };
        assert!(m.validate_format().is_err());
    }

    #[test]
    fn language_matrix_catalog_il_round_trip() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/plasm_language_matrix");
        let cgs = load_schema_dir(&dir).expect("load plasm_language_matrix");
        let hash_before = cgs.catalog_cgs_hash_hex();
        let bytes = cgs_to_catalog_il_bytes(&cgs).expect("encode language matrix");
        let decoded =
            load_catalog_il_verified(&bytes, &hash_before).expect("decode language matrix");
        assert_eq!(decoded.catalog_cgs_hash_hex(), hash_before);
    }
}
