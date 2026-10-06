//! Build one compiled JSON catalog artifact per `apis/<name>/` tree for `--catalog-dir` runtime loading.
//!
//! Usage (from repo root):
//!   cargo run -p plasm --bin plasm-pack-catalogs -- --apis-root apis --output-dir target/plasm-catalogs
//!
//! Hosted Docker builds use `--package-list deploy/saas-packaged-apis.txt`; OSS release tarballs use
//! `plasm-oss/scripts/oss-packaged-apis.txt`.

use clap::Parser;
use plasm_compile::{
    compile_cgs_capability_templates, load_compiled_catalog_artifact,
    validate_cgs_capability_templates, validate_cgs_views,
};
use plasm_core::catalog_il::{
    catalog_artifact_stem, catalog_il_body_name, cgs_to_catalog_il_bytes, CatalogManifest,
    PLASM_CATALOG_FORMAT_VERSION,
};
use plasm_core::loader::{finalize_cgs_load, load_schema_dir_unvalidated};
use plasm_core::schema::CGS;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

type Result<T> = std::result::Result<T, PackError>;

#[derive(Debug, Error)]
enum PackError {
    #[error("filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("catalog artifact JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("embedding acquisition failed: {0}")]
    Embedding(#[from] plasm_agent::discovery_embeddings::EmbeddingAcquisitionError),
    #[error("catalog {stage} failed for `{entry_id}`: {source}")]
    CatalogDiagnostic {
        stage: CatalogStage,
        entry_id: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("CGS entry_id `{actual}` does not match directory name `{expected}`")]
    EntryIdMismatch { actual: String, expected: String },
    #[error("CGS version must be explicitly set (> 0) for `{entry_id}`")]
    MissingVersion { entry_id: String },
    #[error("invalid package list entry `{entry}` in {path}")]
    InvalidPackageListEntry { entry: String, path: PathBuf },
    #[error("package list {0} is empty after removing comments and blanks")]
    EmptyPackageList(PathBuf),
    #[error("discovery artifact hash mismatch for `{entry_id}`")]
    DiscoveryHashMismatch { entry_id: String },
    #[error("duplicate catalog `{entry_id}` in publication")]
    DuplicateCatalog { entry_id: String },
    #[error("publication manifest filename is not valid UTF-8: {0}")]
    InvalidManifestFilename(PathBuf),
    #[error("--package-list: no usable apis/<name>/ under {apis_root} for: {missing}")]
    MissingPackagedCatalogs { apis_root: PathBuf, missing: String },
    #[error("no API packages under {apis_root}{package_list_hint}")]
    NoCatalogsFound {
        apis_root: PathBuf,
        package_list_hint: &'static str,
    },
}

#[derive(Debug, Clone, Copy, Error)]
enum CatalogStage {
    #[error("schema load")]
    SchemaLoad,
    #[error("capability validation")]
    CapabilityValidation,
    #[error("OpenAPI pagination validation")]
    OpenApiPagination,
    #[error("view validation")]
    ViewValidation,
    #[error("CGS validation")]
    CgsValidation,
    #[error("catalog set read")]
    CatalogSetRead,
    #[error("manifest validation")]
    ManifestValidation,
    #[error("catalog artifact read")]
    CatalogArtifactRead,
    #[error("compiled artifact read")]
    CompiledArtifactRead,
    #[error("discovery artifact read")]
    DiscoveryArtifactRead,
    #[error("capability document rendering")]
    CapabilityDocumentRendering,
    #[error("discovery validation")]
    DiscoveryValidation,
    #[error("CGS JSON IL encoding")]
    CgsJsonIlEncoding,
    #[error("request recipe compilation")]
    RequestRecipeCompilation,
}

fn catalog_failure(
    stage: CatalogStage,
    entry_id: impl Into<String>,
    source: impl std::error::Error + Send + Sync + 'static,
) -> PackError {
    PackError::CatalogDiagnostic {
        stage,
        entry_id: entry_id.into(),
        source: Box::new(source),
    }
}

#[derive(clap::Parser, Debug)]
#[command(name = "plasm-pack-catalogs")]
struct Args {
    /// Root directory whose subdirs contain `domain.yaml` + `mappings.yaml` (e.g. repo `apis/`).
    #[arg(long, default_value = "apis")]
    apis_root: PathBuf,

    /// Directory to receive `<entry_id>.v<version>.<hash12>.cgs.json` + `.manifest.json` artifacts.
    #[arg(long, default_value = "target/plasm-catalogs")]
    output_dir: PathBuf,

    /// Cargo workspace root (contains root `Cargo.toml`).
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    /// Persistent vector cache shared across output directories and CI releases.
    #[arg(long)]
    embedding_cache_dir: Option<PathBuf>,

    /// Rebuild artifact metadata; matching cached vectors are still reused.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    force: bool,

    /// Only pack APIs listed in this file (one `apis/<name>/` directory name per line; `#` starts a
    /// comment; blank lines ignored). When omitted, every subdirectory of `--apis-root` with
    /// `domain.yaml` + `mappings.yaml` is packed (local dev default).
    #[arg(long)]
    package_list: Option<PathBuf>,
}

fn packed_json_name(entry_id: &str, version: u64, cgs_hash_hex: &str) -> String {
    catalog_il_body_name(entry_id, version, cgs_hash_hex)
}

fn prepare_cgs_for_catalog(api_dir: &Path, entry_id: &str) -> Result<CGS> {
    let mut cgs = load_schema_dir_unvalidated(api_dir)
        .map_err(|error| catalog_failure(CatalogStage::SchemaLoad, entry_id, error))?;
    validate_cgs_capability_templates(&cgs)
        .map_err(|error| catalog_failure(CatalogStage::CapabilityValidation, entry_id, error))?;
    plasm_compile::validate_catalog_openapi_pagination(&cgs, api_dir)
        .map_err(|error| catalog_failure(CatalogStage::OpenApiPagination, entry_id, error))?;
    validate_cgs_views(&cgs)
        .map_err(|error| catalog_failure(CatalogStage::ViewValidation, entry_id, error))?;

    if let Some(ref eid) = cgs.entry_id {
        if eid != entry_id {
            return Err(PackError::EntryIdMismatch {
                actual: eid.clone(),
                expected: entry_id.to_owned(),
            });
        }
    }

    cgs.bind_registry_entry_id(entry_id);
    if cgs.version == 0 {
        return Err(PackError::MissingVersion {
            entry_id: entry_id.to_owned(),
        });
    }

    finalize_cgs_load(&mut cgs)
        .map_err(|error| catalog_failure(CatalogStage::CgsValidation, entry_id, error))?;

    Ok(cgs)
}

fn load_package_list(path: &Path) -> Result<HashSet<String>> {
    let raw = fs::read_to_string(path)?;
    let mut out = HashSet::new();
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.contains('/') || line.contains('\\') || line.contains("..") {
            return Err(PackError::InvalidPackageListEntry {
                entry: line.to_owned(),
                path: path.to_owned(),
            });
        }
        out.insert(line.to_string());
    }
    if out.is_empty() {
        return Err(PackError::EmptyPackageList(path.to_owned()));
    }
    Ok(out)
}

/// Reuse portable format-3 artifacts even if the local pack stamps/cache were removed.
async fn reusable_catalogs(
    out_dir: &Path,
    embedding_cache: &Path,
) -> Result<std::collections::BTreeMap<String, (String, CatalogManifest)>> {
    let mut reusable = std::collections::BTreeMap::new();
    if !out_dir.join("catalog-set.json").exists() {
        return Ok(reusable);
    }
    for path in plasm_core::catalog_il::read_catalog_set(out_dir)
        .map_err(|error| catalog_failure(CatalogStage::CatalogSetRead, "publication", error))?
    {
        let manifest: CatalogManifest = match serde_json::from_slice(&fs::read(&path)?) {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!(
                    "plasm-pack-catalogs: skip unreadable publication {}: {error}",
                    path.display()
                );
                continue;
            }
        };
        // Packing may replace an older generation; only matching profiles can supply cache hits.
        if manifest.format_version != PLASM_CATALOG_FORMAT_VERSION
            || manifest.embedding_profile != Default::default()
        {
            continue;
        }
        manifest.validate_format().map_err(|error| {
            catalog_failure(CatalogStage::ManifestValidation, &manifest.entry_id, error)
        })?;
        let discovery_bytes = fs::read(out_dir.join(&manifest.discovery_json))?;
        if plasm_core::catalog_discovery::content_hash(&discovery_bytes) != manifest.discovery_hash
        {
            return Err(PackError::DiscoveryHashMismatch {
                entry_id: manifest.entry_id,
            });
        }
        let artifact: plasm_core::catalog_discovery::CatalogDiscoveryArtifact =
            serde_json::from_slice(&discovery_bytes)?;
        if artifact.renderer_version != plasm_core::catalog_discovery::DISCOVERY_RENDERER_VERSION {
            continue;
        }
        let cgs =
            plasm_core::catalog_il::load_catalog_artifact(out_dir, &manifest).map_err(|error| {
                catalog_failure(CatalogStage::CatalogArtifactRead, &manifest.entry_id, error)
            })?;
        load_compiled_catalog_artifact(out_dir, &manifest, &cgs).map_err(|error| {
            catalog_failure(
                CatalogStage::CompiledArtifactRead,
                &manifest.entry_id,
                error,
            )
        })?;
        let artifact = plasm_core::catalog_il::load_discovery_artifact(out_dir, &manifest, &cgs)
            .map_err(|error| {
                catalog_failure(
                    CatalogStage::DiscoveryArtifactRead,
                    &manifest.entry_id,
                    error,
                )
            })?;
        plasm_agent::discovery_embeddings::cache_artifact(&artifact, embedding_cache).await?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| PackError::InvalidManifestFilename(path.clone()))?
            .to_string();
        let entry = manifest.entry_id.clone();
        if reusable.insert(entry.clone(), (name, manifest)).is_some() {
            return Err(PackError::DuplicateCatalog { entry_id: entry });
        }
    }
    Ok(reusable)
}

/// Validate the complete publication before any embedding acquisition or artifact writes.
fn prepare_catalogs(
    apis_root: &Path,
    allowed: Option<&HashSet<String>>,
) -> Result<Vec<(String, CGS)>> {
    let mut catalogs = Vec::new();
    let mut seen_allowed = HashSet::new();
    let mut paths = fs::read_dir(apis_root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    for path in paths {
        if !path.is_dir() {
            continue;
        }
        let domain = path.join("domain.yaml");
        let mappings = path.join("mappings.yaml");
        if !domain.is_file() || !mappings.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }

        if let Some(allow) = allowed {
            if !allow.contains(name) {
                continue;
            }
            seen_allowed.insert(name.to_string());
        }

        catalogs.push((name.to_string(), prepare_cgs_for_catalog(&path, name)?));
    }
    if let Some(allow) = allowed {
        let mut missing: Vec<&String> = allow.difference(&seen_allowed).collect();
        if !missing.is_empty() {
            missing.sort();
            let msg = missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(PackError::MissingPackagedCatalogs {
                apis_root: apis_root.to_owned(),
                missing: msg,
            });
        }
    }

    if catalogs.is_empty() {
        return Err(PackError::NoCatalogsFound {
            apis_root: apis_root.to_owned(),
            package_list_hint: if allowed.is_some() {
                " (check --package-list)"
            } else {
                ": expected subdirs with domain.yaml and mappings.yaml"
            },
        });
    }

    Ok(catalogs)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    pack(args).await
}

async fn pack(args: Args) -> Result<()> {
    let workspace = fs::canonicalize(&args.workspace)?;
    let apis_root = if args.apis_root.is_absolute() {
        args.apis_root.clone()
    } else {
        workspace.join(&args.apis_root)
    };
    let apis_root = fs::canonicalize(apis_root)?;

    let out_dir = if args.output_dir.is_absolute() {
        args.output_dir.clone()
    } else {
        workspace.join(&args.output_dir)
    };
    fs::create_dir_all(&out_dir)?;

    let allowed: Option<HashSet<String>> = match &args.package_list {
        Some(p) => {
            let path = if p.is_absolute() {
                p.clone()
            } else {
                workspace.join(p)
            };
            Some(load_package_list(&path)?)
        }
        None => None,
    };
    if let Some(ref allow) = allowed {
        eprintln!(
            "plasm-pack-catalogs: package list enabled ({} entr{})",
            allow.len(),
            if allow.len() == 1 { "y" } else { "ies" }
        );
    }

    let catalogs = prepare_catalogs(&apis_root, allowed.as_ref())?;
    let mut published_manifests = Vec::new();
    let mut packed = 0usize;
    let mut skipped = 0usize;
    let cache_dir = out_dir.join(".plasm-pack-cache");
    fs::create_dir_all(&cache_dir)?;

    let embedding_cache_dir = args
        .embedding_cache_dir
        .as_ref()
        .map(|path| {
            if path.is_absolute() {
                path.clone()
            } else {
                workspace.join(path)
            }
        })
        .unwrap_or_else(|| cache_dir.join("embeddings"));
    let reusable = reusable_catalogs(&out_dir, &embedding_cache_dir).await?;
    let mut embedded_documents = 0usize;
    let mut reused_documents = 0usize;
    let mut embedding_requests = 0usize;

    for (name, cgs) in catalogs {
        let name = name.as_str();
        let cgs_hash = cgs.catalog_cgs_hash_hex();
        let json_name = packed_json_name(name, cgs.version, &cgs_hash);
        let json_dest = out_dir.join(&json_name);
        if !args.force {
            if let Some((manifest_name, _manifest)) = reusable
                .get(name)
                .filter(|(_, manifest)| manifest.cgs_hash == cgs_hash)
            {
                published_manifests.push(manifest_name.clone());
                eprintln!("plasm-pack-catalogs: skip `{name}` (verified catalog hash unchanged; zero embedding requests)");
                skipped += 1;
                packed += 1;
                continue;
            }
        }

        eprintln!("plasm-pack-catalogs: packing `{name}` …");
        let json_bytes = cgs_to_catalog_il_bytes(&cgs)
            .map_err(|error| catalog_failure(CatalogStage::CgsJsonIlEncoding, name, error))?;
        plasm_agent::discovery_embeddings::atomic_write(&json_dest, &json_bytes)?;

        let recipes = compile_cgs_capability_templates(&cgs).map_err(|error| {
            catalog_failure(CatalogStage::RequestRecipeCompilation, name, error)
        })?;
        let recipes_bytes = serde_json::to_vec(&recipes)?;
        let recipes_hash = plasm_core::catalog_discovery::content_hash(&recipes_bytes);
        let recipes_json = format!(
            "{}.{}.recipes.json",
            catalog_artifact_stem(name, cgs.version, &cgs_hash),
            recipes_hash
        );
        plasm_agent::discovery_embeddings::atomic_write(
            &out_dir.join(&recipes_json),
            &recipes_bytes,
        )?;

        let documents =
            plasm_core::catalog_discovery::capability_documents(&cgs).map_err(|error| {
                catalog_failure(CatalogStage::CapabilityDocumentRendering, name, error)
            })?;
        let embedded =
            plasm_agent::discovery_embeddings::embed_documents(documents, &embedding_cache_dir)
                .await?;
        reused_documents += embedded.reused_documents;
        embedded_documents += embedded.embedded_documents;
        embedding_requests += embedded.requests;
        eprintln!("plasm-pack-catalogs: `{name}` reused {} document vectors, embedded {} documents in {} request(s)",
            embedded.reused_documents, embedded.embedded_documents, embedded.requests);
        let discovery = plasm_core::catalog_discovery::CatalogDiscoveryArtifact {
            entry_id: name.to_string(),
            cgs_hash: cgs_hash.clone(),
            renderer_version: plasm_core::catalog_discovery::DISCOVERY_RENDERER_VERSION,
            profile: Default::default(),
            capabilities: embedded.capabilities,
            prerequisites: cgs.prerequisites.clone(),
        };
        discovery
            .validate(&cgs)
            .map_err(|error| catalog_failure(CatalogStage::DiscoveryValidation, name, error))?;
        let discovery_bytes = serde_json::to_vec(&discovery)?;
        let discovery_hash = plasm_core::catalog_discovery::content_hash(&discovery_bytes);
        let discovery_json = format!(
            "{}.{}.discovery.json",
            catalog_artifact_stem(name, cgs.version, &cgs_hash),
            discovery_hash
        );
        plasm_agent::discovery_embeddings::atomic_write(
            &out_dir.join(&discovery_json),
            &discovery_bytes,
        )?;
        let label = cgs.entry_id.clone().unwrap_or_else(|| name.to_string());
        let manifest = CatalogManifest {
            format_version: PLASM_CATALOG_FORMAT_VERSION,
            entry_id: name.to_string(),
            version: cgs.version,
            cgs_hash: cgs_hash.clone(),
            label,
            tags: vec![],
            cgs_json: json_name,
            recipes_json,
            recipes_hash,
            discovery_json,
            discovery_hash,
            embedding_profile: discovery.profile,
        };
        manifest
            .validate_format()
            .map_err(|error| catalog_failure(CatalogStage::ManifestValidation, name, error))?;
        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        let manifest_name = format!(
            "{}.{}.manifest.json",
            catalog_artifact_stem(name, cgs.version, &cgs_hash),
            manifest.discovery_hash
        );
        let manifest_dest = out_dir.join(&manifest_name);
        plasm_agent::discovery_embeddings::atomic_write(&manifest_dest, manifest_json.as_bytes())?;

        published_manifests.push(manifest_name);

        eprintln!(
            "plasm-pack-catalogs: wrote {} (catalog hash {})",
            json_dest.display(),
            cgs_hash
        );
        packed += 1;
    }

    published_manifests.sort();
    let set = plasm_core::catalog_il::CatalogSetManifest {
        format_version: PLASM_CATALOG_FORMAT_VERSION,
        manifests: published_manifests,
    };
    plasm_agent::discovery_embeddings::atomic_write(
        &out_dir.join("catalog-set.json"),
        &serde_json::to_vec_pretty(&set)?,
    )?;

    eprintln!("plasm-pack-catalogs: embedding acquisition: {embedded_documents} new documents, {reused_documents} cached documents, {embedding_requests} requests; {skipped} whole catalogs reused");

    if skipped > 0 {
        eprintln!(
            "plasm-pack-catalogs: packed {packed} catalog(s) into {} (reused {skipped} unchanged)",
            out_dir.display()
        );
    } else {
        eprintln!(
            "plasm-pack-catalogs: packed {packed} catalog(s) into {}",
            out_dir.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::prerequisites::{prerequisite_closure, CapabilityRef, DeploymentBindings};
    use std::collections::{BTreeMap, BTreeSet};

    #[test]
    fn catalog_failure_preserves_stage_and_concrete_source_chain() {
        use std::error::Error as _;
        let error = catalog_failure(
            CatalogStage::CatalogArtifactRead,
            "fixture",
            plasm_core::catalog_il::CatalogIlError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing artifact",
            )),
        );
        assert!(matches!(
            &error,
            PackError::CatalogDiagnostic {
                stage: CatalogStage::CatalogArtifactRead,
                entry_id,
                ..
            } if entry_id == "fixture"
        ));
        let source = error.source().unwrap();
        assert!(source.is::<plasm_core::catalog_il::CatalogIlError>());
        assert_eq!(
            source
                .source()
                .unwrap()
                .downcast_ref::<std::io::Error>()
                .unwrap()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(error
            .to_string()
            .contains("catalog artifact read failed for `fixture`"));
    }

    #[test]
    fn pack_preflight_preserves_schema_load_source() {
        use std::error::Error as _;
        let directory = tempfile::tempdir().unwrap();
        let error = prepare_cgs_for_catalog(directory.path(), "fixture").unwrap_err();
        assert!(matches!(
            &error,
            PackError::CatalogDiagnostic {
                stage: CatalogStage::SchemaLoad,
                ..
            }
        ));
        assert!(error
            .source()
            .unwrap()
            .is::<plasm_core::loader::SchemaLoadError>());
    }

    #[test]
    fn package_list_rejections_are_semantic_errors() {
        let dir = tempfile::tempdir().unwrap();
        let invalid = dir.path().join("invalid.txt");
        fs::write(&invalid, "api/child\n").unwrap();
        assert!(matches!(
            load_package_list(&invalid),
            Err(PackError::InvalidPackageListEntry { entry, .. }) if entry == "api/child"
        ));

        let empty = dir.path().join("empty.txt");
        fs::write(&empty, "# comment\n  \n").unwrap();
        assert!(matches!(
            load_package_list(&empty),
            Err(PackError::EmptyPackageList(path)) if path == empty
        ));
    }

    #[test]
    fn all_catalog_packages_pass_pack_preflight() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis");
        let mut errors = Vec::new();
        for entry in fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            if !entry.path().join("domain.yaml").is_file() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Err(error) = prepare_cgs_for_catalog(&entry.path(), &name) {
                errors.push(format!("{name}: {error:#}"));
            }
        }
        errors.sort();
        assert!(
            errors.is_empty(),
            "Catalog preflight failures:\n{}",
            errors.join("\n")
        );
    }

    fn request(catalog: &str, capability: &str, inputs: &[(&str, &str)]) -> serde_json::Value {
        let inputs: serde_json::Map<String, serde_json::Value> = inputs
            .iter()
            .map(|(key, value)| {
                (
                    key.to_string(),
                    serde_json::Value::String(value.to_string()),
                )
            })
            .collect();
        request_with_inputs(catalog, capability, serde_json::Value::Object(inputs))
    }

    fn request_with_inputs(
        catalog: &str,
        capability: &str,
        inputs: serde_json::Value,
    ) -> serde_json::Value {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis");
        let cgs = prepare_cgs_for_catalog(&root.join(catalog), catalog)
            .unwrap_or_else(|e| panic!("{catalog}: {e:#}"));
        let cap = cgs
            .capabilities
            .values()
            .find(|c| c.name.as_str() == capability)
            .unwrap();
        let template =
            plasm_compile::parse_capability_template(&cap.require_mapping().unwrap().template.0)
                .unwrap();
        let env = serde_json::from_value(inputs).unwrap();
        let plasm_compile::CompiledOperation::Http(request) =
            plasm_compile::compile_operation(&template, &env).unwrap()
        else {
            panic!("expected HTTP catalog transport");
        };
        request.to_json()
    }

    #[test]
    fn compound_relation_queries_use_scalar_identity_components() {
        for (catalog, parent, relation_name) in [
            ("google-drive", "DriveComment", "replies"),
            ("vultr", "KubernetesNodePool", "vke_labels"),
            ("vultr", "KubernetesNodePool", "vke_taints"),
        ] {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis");
            let cgs = prepare_cgs_for_catalog(&root.join(catalog), catalog).unwrap();
            let entity = cgs.get_entity(parent).unwrap();
            let relation = entity.relations.get(relation_name).unwrap();
            let Some(plasm_core::RelationMaterialization::QueryScopedBindings {
                capability,
                bindings,
            }) = relation.materialize.as_ref()
            else {
                panic!("expected compound relation bindings for {catalog}/{relation_name}");
            };
            cgs.validate_relation_materialize_bindings(
                parent,
                relation_name,
                entity,
                capability,
                bindings,
            )
            .unwrap();
            assert_eq!(bindings.len(), 2, "{catalog}/{relation_name}");
        }
        assert_eq!(
            request(
                "google-drive",
                "replies_list",
                &[("fileId", "file-1"), ("commentId", "comment-1")]
            )["path"],
            "/files/file-1/comments/comment-1/replies"
        );
        assert_eq!(
            request(
                "vultr",
                "kubernetesnodepoollabel_query",
                &[("vke_id", "cluster-1"), ("nodepool_id", "pool-1")]
            )["path"],
            "/v2/kubernetes/clusters/cluster-1/node-pools/pool-1/labels"
        );
    }

    #[test]
    fn repaired_catalog_inputs_preserve_wire_semantics() {
        let issue = request(
            "gitlab",
            "issue_for_project_query",
            &[
                ("shelf", "issue_for_project"),
                ("project_id", "42"),
                ("order_by", "updated_at"),
                ("sort", "desc"),
            ],
        );
        assert_eq!(issue["path"], "/api/v4/projects/42/issues");
        assert_eq!(issue["query"]["order_by"], "updated_at");
        assert_eq!(issue["query"]["sort"], "desc");
        assert!(issue["query"].get("project_id").is_none());
        let tables = request(
            "grafana",
            "datasource_clickhouse_table_query",
            &[
                ("id", "ds-1"),
                ("ds_type", "grafana-clickhouse-datasource"),
                ("database", "db' OR 1=1 --"),
            ],
        );
        let sql = tables["body"]["queries"][0]["rawSql"].as_str().unwrap();
        assert!(sql.contains("base64Decode('"));
        assert!(!sql.contains("OR 1=1"));
        let comment = request_with_inputs(
            "figma",
            "comment_create",
            serde_json::json!({
                "file_key": "file-1", "input": {"message": "Review", "comment_id": "parent-1", "client_meta": {"x": 1, "y": 2}}
            }),
        );
        assert_eq!(comment["path"], "/v1/files/file-1/comments");
        assert_eq!(comment["body"]["comment_id"], "parent-1");
        assert_eq!(comment["body"]["client_meta"]["x"], 1);
    }

    #[test]
    fn gmail_generic_mail_mapping_is_deterministic() {
        use base64::Engine as _;
        let inputs = [
            ("sender", "sender@example.test"),
            ("to", "recipient@example.test"),
            ("subject", "A subject"),
            ("plainBody", "A body"),
        ];
        let first = request("gmail", "message_send_simple", &inputs);
        assert_eq!(first, request("gmail", "message_send_simple", &inputs));
        let raw = first["body"]["raw"].as_str().unwrap();
        let mail = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(raw)
                .unwrap(),
        )
        .unwrap();
        assert!(mail.contains("from: sender@example.test\r\n"));
        assert!(!mail.contains("date:"));
        assert!(!mail.contains("message-id:"));
        assert!(first["body"].get("threadId").is_none());
        let reply = request(
            "gmail",
            "message_reply",
            &[
                ("sender", "sender@example.test"),
                ("plainBody", "Reply body"),
                ("parent_headerFrom", "recipient@example.test"),
                ("parent_headerSubject", "A subject"),
                ("parent_headerMessageId", "<parent@example.test>"),
                ("parent_threadId", "thread-a"),
            ],
        );
        let mail = String::from_utf8(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(reply["body"]["raw"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert!(mail.contains("in-reply-to: <parent@example.test>\r\n"));
        assert!(mail.contains("subject: Re: A subject\r\n"));
        assert_eq!(reply["body"]["threadId"], "thread-a");
    }

    #[test]
    fn appworld_auth_uses_shared_bearer_declarations() {
        for app in [
            "amazon",
            "file_system",
            "gmail",
            "phone",
            "simple_note",
            "splitwise",
            "spotify",
            "todoist",
            "venmo",
        ] {
            let request = request(
                &format!("appworld/{app}"),
                "logout",
                &[("access_token", "synthetic-token")],
            );
            assert_eq!(
                request["headers"]["Authorization"], "Bearer synthetic-token",
                "{app}"
            );
        }
    }

    #[test]
    fn proof_credential_templates_compile_without_host_dispatch() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        let cgs = prepare_cgs_for_catalog(&root, "proof").unwrap();
        let bindings: DeploymentBindings =
            serde_json::from_slice(&fs::read(root.join("deployment-bindings.json")).unwrap())
                .unwrap();
        let business: Vec<_> = bindings
            .bindings
            .iter()
            .map(|binding| binding.consumer.clone())
            .collect();
        let closure = plasm_core::prerequisites::prerequisite_closure(
            &BTreeMap::from([("proof".into(), &cgs)]),
            &bindings,
            &business,
            &BTreeSet::from(["proof".into()]),
        )
        .unwrap();
        assert_eq!(closure.edges.len(), bindings.bindings.len());
        assert!(closure
            .prerequisites
            .iter()
            .all(|cap| cap.capability == "editor_state_get"));
        let cap = cgs.get_capability("document_share_bind").unwrap();
        let template =
            plasm_compile::parse_capability_template(&cap.require_mapping().unwrap().template.0)
                .unwrap();
        let env = serde_json::from_value(serde_json::json!({"document":"record-a"})).unwrap();
        let plasm_compile::CompiledOperation::CredentialBind(binding) =
            plasm_compile::compile_operation(&template, &env).unwrap()
        else {
            panic!("expected declared local effect")
        };
        assert_eq!(binding.resource, serde_json::json!({"document":"record-a"}));
        assert_eq!(binding.source, plasm_compile::CredentialSource::Host {});
        assert!(serde_json::to_value(&binding)
            .unwrap()
            .get("secret")
            .is_none());
        for document in [
            "../other",
            "a/../b",
            "https://untrusted.test/path",
            "a?query=value",
            "a#fragment",
        ] {
            let env = serde_json::from_value(serde_json::json!({"document":document})).unwrap();
            assert!(
                plasm_compile::compile_operation(&template, &env).is_err(),
                "binding must validate document identity: {document}"
            );
        }
        let request = request(
            "proof",
            "editor_state_get",
            &[
                ("slug", "record-a"),
                ("access", "cr00000000000000000000000000000000"),
            ],
        );
        assert_eq!(
            request["credential"]["resource"],
            serde_json::json!({"document":"record-a"})
        );
        assert!(request
            .get("query")
            .is_none_or(|query| query.get("token").is_none()));
    }

    #[test]
    fn catalog_query_cutover_preserves_vendor_requests() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/musixmatch");
        let cgs = prepare_cgs_for_catalog(&root, "musixmatch").unwrap();
        for source in [
            r#"Track{mode="search", q_artist="composer"}"#,
            r#"Track{mode="chart", country="gb"}"#,
        ] {
            plasm_core::expr_parser::parse(source, &cgs)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
        }

        let artist = request("musixmatch", "artist_search", &[("query", "composer")]);
        assert_eq!(artist["path"], "/artist.search");
        assert_eq!(artist["query"]["q_artist"], "composer");
        let tracks = request(
            "musixmatch",
            "track_query",
            &[("mode", "search"), ("q_artist", "composer")],
        );
        assert_eq!(tracks["path"], "/track.search");
        assert_eq!(tracks["query"]["q_artist"], "composer");
        let chart = request(
            "musixmatch",
            "track_query",
            &[("mode", "chart"), ("country", "gb")],
        );
        assert_eq!(chart["path"], "/chart.tracks.get");
        assert_eq!(chart["query"]["country"], "gb");
        let movie = request("omdb", "movie_search", &[("query", "example")]);
        assert_eq!(movie["query"]["s"], "example");
        let pages = request("notion", "database_query", &[]);
        assert_eq!(pages["path"], "/v1/search");
        assert_eq!(pages["body"]["filter"]["value"], "page");
        assert!(pages["body"].get("query").is_none());
        let pages = request("notion", "database_query", &[("query", "example")]);
        assert_eq!(pages["body"]["query"], "example");
        let rows = request("notion", "database_query", &[("database_id", "db-123")]);
        assert_eq!(rows["path"], "/v1/databases/db-123/query");
        assert!(rows["body"].get("filter").is_none());
        let databases = request("notion", "database_search", &[]);
        assert_eq!(databases["body"]["filter"]["value"], "database");
        let changes = request(
            "google-drive",
            "changes_list",
            &[("startPageToken", "initial")],
        );
        assert_eq!(changes["query"]["pageToken"], "initial");
        assert!(changes["query"].get("startPageToken").is_none());
    }

    #[test]
    fn catalog_parent_and_listing_contracts_compile() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/clickup");
        let cgs = prepare_cgs_for_catalog(&root, "clickup").unwrap();
        for source in [
            r#"Space{team_id="123", archived=false}"#,
            r#"Task{shelf="list_task", list_id="123", archived=false, include_closed=true, subtasks=true}"#,
        ] {
            plasm_core::expr_parser::parse(source, &cgs)
                .unwrap_or_else(|error| panic!("{source}: {error}"));
        }
        let tasks = request_with_inputs(
            "clickup",
            "list_task_query",
            serde_json::json!({
                "shelf":"list_task", "list_id":"123", "archived":false,
                "include_closed":true, "subtasks":true
            }),
        );
        assert_eq!(tasks["query"]["archived"], false);
        assert_eq!(tasks["query"]["include_closed"], true);
        assert_eq!(tasks["query"]["subtasks"], true);
        for (capability, input, path) in [
            ("team_create_space", "team_id", "/v2/team/123/space"),
            ("space_create_folder", "space_id", "/v2/space/123/folder"),
            ("folder_create_list", "folder_id", "/v2/folder/123/list"),
        ] {
            let compiled = request_with_inputs(
                "clickup",
                capability,
                serde_json::json!({input: "123", "name": "example", "input": {"name": "example"}}),
            );
            assert_eq!(compiled["path"], path);
            assert_eq!(compiled["body"]["name"], "example");
            assert!(compiled["body"].get(input).is_none());
        }
        for (capability, shelf, input, path) in [
            (
                "space_views",
                "folder_views",
                "folder_id",
                "/v2/folder/123/view",
            ),
            ("space_views", "view", "team_id", "/v2/team/123/view"),
            (
                "space_views",
                "space_views",
                "space_id",
                "/v2/space/123/view",
            ),
            ("list_query", "folder", "folder_id", "/v2/folder/123/list"),
            ("list_query", "list", "space_id", "/v2/space/123/list"),
            (
                "list_task_query",
                "list_task",
                "list_id",
                "/v2/list/123/task",
            ),
            (
                "list_task_query",
                "view_tasks",
                "view_id",
                "/v2/view/123/task",
            ),
            ("list_task_query", "task", "team_id", "/v2/team/123/task"),
        ] {
            let compiled = request("clickup", capability, &[("shelf", shelf), (input, "123")]);
            assert_eq!(compiled["path"], path);
            assert!(compiled["query"].get(input).is_none());
        }
        let groups = request("clickup", "group_query", &[("team_id", "123")]);
        assert_eq!(groups["query"]["team_id"], "123");
        let dependency = request(
            "clickup",
            "task_remove_dependency",
            &[
                ("id", "task-a"),
                ("depends_on", "task-b"),
                ("dependency_of", "task-c"),
            ],
        );
        assert_eq!(dependency["path"], "/v2/task/task-a/dependency");
        assert_eq!(dependency["query"]["depends_on"], "task-b");
        assert_eq!(dependency["query"]["dependency_of"], "task-c");
        let tickers = request(
            "architect-exchange",
            "ticker_query",
            &[("sort", "symbol:desc")],
        );
        assert_eq!(tickers["path"], "/api/tickers");
        assert_eq!(tickers["query"]["sort"], "symbol:desc");
        let ticker = request("architect-exchange", "ticker_get", &[("symbol", "EXAMPLE")]);
        assert_eq!(ticker["path"], "/api/ticker");
        assert_eq!(ticker["query"]["symbol"], "EXAMPLE");
        let rulesets = request("cloudflare", "ruleset_query", &[("zone_id", "zone-a")]);
        assert_eq!(rulesets["path"], "/zones/zone-a/rulesets");
        assert!(rulesets["query"].get("per_page").is_none());
    }

    #[test]
    fn appworld_declared_prerequisite_generation() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apis/appworld");
        let names = [
            "supervisor",
            "amazon",
            "file_system",
            "gmail",
            "phone",
            "simple_note",
            "splitwise",
            "spotify",
            "todoist",
            "venmo",
        ];
        let catalogs: BTreeMap<_, _> = names
            .into_iter()
            .map(|name| {
                (
                    name.to_string(),
                    prepare_cgs_for_catalog(&root.join(name), name)
                        .unwrap_or_else(|e| panic!("{name}: {e:#}")),
                )
            })
            .collect();
        let refs = catalogs.iter().map(|(id, cgs)| (id.clone(), cgs)).collect();
        let allowed: BTreeSet<_> = catalogs.keys().cloned().collect();
        let bindings: DeploymentBindings =
            serde_json::from_slice(&fs::read(root.join("deployment-bindings.json")).unwrap())
                .unwrap();
        // Validate the complete deployment, including capabilities that discovery
        // may not select in the representative closure below.
        let all_consumers: Vec<_> = catalogs
            .iter()
            .flat_map(|(catalog, cgs)| {
                cgs.prerequisites
                    .requirements
                    .keys()
                    .map(move |capability| CapabilityRef {
                        catalog: catalog.clone(),
                        capability: capability.clone(),
                    })
            })
            .collect();
        prerequisite_closure(&refs, &bindings, &all_consumers, &allowed).unwrap();
        let business: Vec<_> = catalogs
            .iter()
            .filter(|(id, _)| id.as_str() != "supervisor")
            .map(|(id, cgs)| {
                let (capability, _) = cgs
                    .prerequisites
                    .requirements
                    .iter()
                    .find(|(_, rs)| rs.iter().any(|r| r.id == "session"))
                    .unwrap();
                CapabilityRef {
                    catalog: id.clone(),
                    capability: capability.clone(),
                }
            })
            .collect();
        let closure = prerequisite_closure(&refs, &bindings, &business, &allowed).unwrap();
        assert_eq!(closure.business.len(), 9);
        assert_eq!(
            closure
                .acquisitions
                .iter()
                .filter(|a| a.provider_catalog == "supervisor" && a.provider == "principal")
                .count(),
            1
        );
        assert_eq!(
            closure
                .acquisitions
                .iter()
                .filter(|a| a.provider == "password")
                .count(),
            9
        );
        assert_eq!(
            closure
                .acquisitions
                .iter()
                .filter(|a| a.provider == "session")
                .count(),
            9
        );
        let phone = closure
            .edges
            .iter()
            .find(|e| e.consumer.catalog == "phone" && e.requirement.id == "principal")
            .unwrap();
        assert_eq!(phone.requirement.bindings[0].output, "phone");
        let all: Vec<_> = catalogs
            .iter()
            .flat_map(|(id, cgs)| {
                cgs.capabilities.keys().map(|capability| CapabilityRef {
                    catalog: id.clone(),
                    capability: capability.to_string(),
                })
            })
            .collect();
        prerequisite_closure(&refs, &bindings, &all, &allowed).unwrap();
    }

    #[tokio::test]
    async fn whole_catalog_reuse_recovers_vectors_without_stamps_or_credentials() {
        let output = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/prerequisite_matrix");
        let mut cgs = plasm_core::loader::load_schema_dir_unvalidated(&fixture).unwrap();
        cgs.bind_registry_entry_id("prerequisite_matrix");
        finalize_cgs_load(&mut cgs).unwrap();
        let cgs_hash = cgs.catalog_cgs_hash_hex();
        let documents = plasm_core::catalog_discovery::capability_documents(&cgs).unwrap();
        let artifact = plasm_core::catalog_discovery::CatalogDiscoveryArtifact {
            entry_id: "prerequisite_matrix".into(),
            cgs_hash: cgs_hash.clone(),
            renderer_version: plasm_core::catalog_discovery::DISCOVERY_RENDERER_VERSION,
            profile: Default::default(),
            prerequisites: cgs.prerequisites.clone(),
            capabilities: documents
                .clone()
                .into_iter()
                .map(
                    |document| plasm_core::catalog_discovery::EmbeddedCapability {
                        document,
                        embedding: vec![0.375; 1536],
                    },
                )
                .collect(),
        };
        let discovery = serde_json::to_vec(&artifact).unwrap();
        let recipes = serde_json::to_vec(&compile_cgs_capability_templates(&cgs).unwrap()).unwrap();
        let recipes_hash = plasm_core::catalog_discovery::content_hash(&recipes);
        let manifest = CatalogManifest {
            format_version: PLASM_CATALOG_FORMAT_VERSION,
            entry_id: "prerequisite_matrix".into(),
            version: cgs.version,
            cgs_hash,
            label: "prerequisite_matrix".into(),
            tags: vec![],
            cgs_json: "fixture.cgs.json".into(),
            recipes_json: "fixture.recipes.json".into(),
            recipes_hash,
            discovery_json: "fixture.discovery.json".into(),
            discovery_hash: plasm_core::catalog_discovery::content_hash(&discovery),
            embedding_profile: Default::default(),
        };
        fs::write(
            output.path().join(&manifest.cgs_json),
            cgs_to_catalog_il_bytes(&cgs).unwrap(),
        )
        .unwrap();
        fs::write(output.path().join(&manifest.recipes_json), &recipes).unwrap();
        fs::write(output.path().join(&manifest.discovery_json), &discovery).unwrap();
        fs::write(
            output.path().join("fixture.manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            output.path().join("catalog-set.json"),
            serde_json::to_vec(&plasm_core::catalog_il::CatalogSetManifest {
                format_version: PLASM_CATALOG_FORMAT_VERSION,
                manifests: vec!["fixture.manifest.json".into()],
            })
            .unwrap(),
        )
        .unwrap();
        for _ in 0..2 {
            let reused = reusable_catalogs(output.path(), cache.path())
                .await
                .unwrap();
            assert_eq!(reused["prerequisite_matrix"].1.cgs_hash, manifest.cgs_hash);
            let embedded =
                plasm_agent::discovery_embeddings::embed_documents(documents.clone(), cache.path())
                    .await
                    .unwrap();
            assert_eq!((embedded.embedded_documents, embedded.requests), (0, 0));
            assert_eq!(embedded.capabilities, artifact.capabilities);
            assert_eq!(
                fs::read(output.path().join(&manifest.discovery_json)).unwrap(),
                discovery
            );
        }
        let package_list = tempfile::NamedTempFile::new().unwrap();
        fs::write(package_list.path(), "prerequisite_matrix\n").unwrap();
        let args = |force| Args {
            apis_root: fixture.parent().unwrap().to_path_buf(),
            output_dir: output.path().to_path_buf(),
            workspace: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            embedding_cache_dir: Some(cache.path().to_path_buf()),
            force,
            package_list: Some(package_list.path().to_path_buf()),
        };
        // Exercise the command, including force, from already acquired portable vectors.
        pack(args(true)).await.unwrap();
        let snapshot = || {
            let mut files = std::collections::BTreeMap::new();
            for entry in fs::read_dir(output.path()).unwrap() {
                let path = entry.unwrap().path();
                if path.is_file() {
                    files.insert(
                        path.clone(),
                        (
                            fs::read(&path).unwrap(),
                            fs::metadata(&path).unwrap().modified().unwrap(),
                        ),
                    );
                }
            }
            files
        };
        let first_publication = snapshot();
        pack(args(false)).await.unwrap();
        pack(args(true)).await.unwrap();
        assert_eq!(
            snapshot(),
            first_publication,
            "repeated packing must preserve artifact bytes and modification times"
        );
        let active = plasm_core::catalog_il::read_catalog_set(output.path()).unwrap();
        let active_manifest = plasm_core::catalog_il::read_catalog_manifest(&active[0]).unwrap();
        let recipe_path = output.path().join(&active_manifest.recipes_json);
        let recipe_bytes = fs::read(&recipe_path).unwrap();
        fs::write(&recipe_path, b"{}").unwrap();
        assert!(
            reusable_catalogs(output.path(), cache.path())
                .await
                .is_err(),
            "damaged compiled recipes must not be published as cache hits"
        );
        fs::write(&recipe_path, recipe_bytes).unwrap();
        fs::write(output.path().join(&active_manifest.discovery_json), b"{}").unwrap();
        assert!(
            reusable_catalogs(output.path(), cache.path())
                .await
                .is_err(),
            "damaged artifacts must not be published as cache hits"
        );
    }

    #[tokio::test]
    async fn complete_set_validation_precedes_acquisition_and_preserves_publication() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join("apis");
        let valid = root.join("prerequisite_matrix");
        fs::create_dir_all(&valid).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/prerequisite_matrix");
        for name in ["domain.yaml", "mappings.yaml"] {
            fs::copy(fixture.join(name), valid.join(name)).unwrap();
        }
        let invalid = root.join("z_invalid");
        fs::create_dir(&invalid).unwrap();
        fs::write(invalid.join("domain.yaml"), "invalid: [").unwrap();
        fs::write(invalid.join("mappings.yaml"), "{}").unwrap();
        let output = workspace.path().join("packed");
        fs::create_dir(&output).unwrap();
        let publication = output.join("catalog-set.json");
        fs::write(&publication, "previous publication").unwrap();
        let cache = workspace.path().join("vectors");
        let failure = pack(Args {
            apis_root: root,
            output_dir: output.clone(),
            workspace: workspace.path().to_path_buf(),
            embedding_cache_dir: Some(cache.clone()),
            force: false,
            package_list: None,
        })
        .await
        .unwrap_err();
        assert!(failure.to_string().contains("z_invalid"), "{failure:#}");
        assert!(
            !cache.exists(),
            "invalid publication must fail before acquisition setup"
        );
        assert_eq!(
            fs::read_to_string(publication).unwrap(),
            "previous publication"
        );
        assert_eq!(fs::read_dir(output).unwrap().count(), 1);
    }
}
