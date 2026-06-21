//! Build one compiled JSON catalog artifact per `apis/<name>/` tree for `--catalog-dir` runtime loading.
//!
//! Usage (from repo root):
//!   cargo run -p plasm --bin plasm-pack-catalogs -- --apis-root apis --output-dir target/plasm-catalogs
//!
//! Hosted Docker builds use `--package-list deploy/saas-packaged-apis.txt`; OSS release tarballs use
//! `plasm-oss/scripts/oss-packaged-apis.txt`.

use anyhow::{bail, Context, Result};
use clap::Parser;
use plasm_compile::{validate_cgs_capability_templates, validate_cgs_views};
use plasm_core::catalog_il::{
    catalog_artifact_stem, catalog_il_body_name, cgs_to_catalog_il_bytes, CatalogManifest,
    PLASM_CATALOG_FORMAT_VERSION,
};
use plasm_core::loader::{finalize_cgs_load, load_schema_dir_unvalidated};
use plasm_core::schema::CGS;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

    /// Rebuild every catalog artifact even when an up-to-date packed artifact already exists.
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

fn packed_manifest_name(entry_id: &str, version: u64, cgs_hash_hex: &str) -> String {
    format!(
        "{}.manifest.json",
        catalog_artifact_stem(entry_id, version, cgs_hash_hex)
    )
}

fn packed_file_version_for_entry(file_name: &str, entry_id: &str) -> Option<u64> {
    let prefix = format!("{entry_id}.v");
    let rest = file_name.strip_prefix(&prefix)?;
    let ver = rest.split('.').next()?;
    ver.parse::<u64>().ok()
}

fn enforce_entry_retention(
    out_dir: &Path,
    entry_id: &str,
    keep: usize,
    prefer: &Path,
) -> Result<()> {
    #[derive(Debug)]
    struct Artifact {
        path: PathBuf,
        version: u64,
        modified: SystemTime,
        preferred: bool,
    }

    let mut artifacts = Vec::<Artifact>::new();
    for ent in fs::read_dir(out_dir).with_context(|| format!("read_dir {}", out_dir.display()))? {
        let ent = ent?;
        let p = ent.path();
        if !p.is_file() {
            continue;
        }
        let Some(file_name) = p.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !file_name.ends_with(".cgs.json") {
            continue;
        }
        let Some(version) = packed_file_version_for_entry(file_name, entry_id) else {
            continue;
        };
        let modified = ent
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        artifacts.push(Artifact {
            preferred: p == prefer,
            path: p,
            version,
            modified,
        });
    }

    artifacts.sort_by(|a, b| {
        b.preferred
            .cmp(&a.preferred)
            .then_with(|| b.version.cmp(&a.version))
            .then_with(|| b.modified.cmp(&a.modified))
            .then_with(|| a.path.cmp(&b.path))
    });

    for old in artifacts.into_iter().skip(keep.max(1)) {
        eprintln!(
            "plasm-pack-catalogs: prune old `{}` artifact {}",
            entry_id,
            old.path.display()
        );
        let stem = old
            .path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|n| n.strip_suffix(".cgs.json"))
            .unwrap_or("");
        let manifest = out_dir.join(format!("{stem}.manifest.json"));
        fs::remove_file(&old.path).with_context(|| format!("remove {}", old.path.display()))?;
        if manifest.is_file() {
            let _ = fs::remove_file(&manifest);
        }
    }
    Ok(())
}

fn prepare_cgs_for_catalog(api_dir: &Path, entry_id: &str) -> Result<CGS> {
    let mut cgs = load_schema_dir_unvalidated(api_dir)
        .map_err(|e| anyhow::anyhow!("load_schema {}: {e}", api_dir.display()))?;
    validate_cgs_capability_templates(&cgs)
        .map_err(|e| anyhow::anyhow!("validate {entry_id}: {e}"))?;
    validate_cgs_views(&cgs).map_err(|e| anyhow::anyhow!("validate views {entry_id}: {e}"))?;

    if let Some(ref eid) = cgs.entry_id {
        if eid != entry_id {
            bail!(
                "CGS entry_id {:?} does not match directory name {:?}",
                eid,
                entry_id
            );
        }
    }

    cgs.entry_id = Some(entry_id.to_string());
    if cgs.version == 0 {
        bail!(
            "CGS version must be explicitly set (> 0) for `{}` (no defaulting)",
            entry_id
        );
    }

    finalize_cgs_load(&cgs).map_err(|e| anyhow::anyhow!("CGS validate {entry_id}: {e}"))?;

    Ok(cgs)
}

fn load_package_list(path: &Path) -> Result<HashSet<String>> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = HashSet::new();
    for line in raw.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.contains('/') || line.contains('\\') || line.contains("..") {
            bail!(
                "invalid package list entry {:?} in {} (expected a single directory name under apis/)",
                line,
                path.display()
            );
        }
        out.insert(line.to_string());
    }
    if out.is_empty() {
        bail!(
            "package list {} is empty after removing comments and blanks",
            path.display()
        );
    }
    Ok(out)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let workspace = fs::canonicalize(&args.workspace).context("workspace path")?;
    let apis_root = if args.apis_root.is_absolute() {
        args.apis_root.clone()
    } else {
        workspace.join(&args.apis_root)
    };
    let apis_root = fs::canonicalize(apis_root).context("apis_root")?;

    let out_dir = if args.output_dir.is_absolute() {
        args.output_dir.clone()
    } else {
        workspace.join(&args.output_dir)
    };
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("create_dir_all {}", out_dir.display()))?;

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

    let mut packed = 0usize;
    let mut skipped = 0usize;
    let mut seen_allowed = HashSet::<String>::new();
    let cache_dir = out_dir.join(".plasm-pack-cache");
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("create_dir_all {}", cache_dir.display()))?;

    for ent in
        fs::read_dir(&apis_root).with_context(|| format!("read_dir {}", apis_root.display()))?
    {
        let ent = ent?;
        let path = ent.path();
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

        if let Some(ref allow) = allowed {
            if !allow.contains(name) {
                continue;
            }
            seen_allowed.insert(name.to_string());
        }

        let cgs = prepare_cgs_for_catalog(&path, name)?;
        let cgs_hash = cgs.catalog_cgs_hash_hex();
        let json_name = packed_json_name(name, cgs.version, &cgs_hash);
        let manifest_name = packed_manifest_name(name, cgs.version, &cgs_hash);
        let json_dest = out_dir.join(&json_name);
        let manifest_dest = out_dir.join(&manifest_name);
        let stamp_path = cache_dir.join(format!("{name}.stamp"));
        let stamp_body = format!("{cgs_hash}\n{PLASM_CATALOG_FORMAT_VERSION}\n");

        if !args.force
            && json_dest.is_file()
            && manifest_dest.is_file()
            && fs::read_to_string(&stamp_path).ok().as_deref() == Some(stamp_body.as_str())
        {
            eprintln!("plasm-pack-catalogs: skip `{name}` (artifact up to date)");
            enforce_entry_retention(&out_dir, name, 1, &json_dest)?;
            skipped += 1;
            packed += 1;
            continue;
        }

        eprintln!("plasm-pack-catalogs: packing `{name}` …");
        let json_bytes = cgs_to_catalog_il_bytes(&cgs)
            .map_err(|e| anyhow::anyhow!("encode CGS JSON IL: {e}"))?;
        fs::write(&json_dest, &json_bytes)
            .with_context(|| format!("write {}", json_dest.display()))?;

        let label = cgs.entry_id.clone().unwrap_or_else(|| name.to_string());
        let manifest = CatalogManifest {
            format_version: PLASM_CATALOG_FORMAT_VERSION,
            entry_id: name.to_string(),
            version: cgs.version,
            cgs_hash: cgs_hash.clone(),
            label,
            tags: vec![],
            cgs_json: json_name,
        };
        manifest
            .validate_format()
            .map_err(|e| anyhow::anyhow!("manifest validate: {e}"))?;
        let manifest_json = serde_json::to_string_pretty(&manifest).context("manifest json")?;
        fs::write(&manifest_dest, manifest_json)
            .with_context(|| format!("write {}", manifest_dest.display()))?;

        fs::write(&stamp_path, &stamp_body)
            .with_context(|| format!("write stamp {}", stamp_path.display()))?;

        eprintln!(
            "plasm-pack-catalogs: wrote {} (catalog hash {})",
            json_dest.display(),
            cgs_hash
        );
        enforce_entry_retention(&out_dir, name, 1, &json_dest)?;
        packed += 1;
    }

    if let Some(ref allow) = allowed {
        let mut missing: Vec<&String> = allow.difference(&seen_allowed).collect();
        if !missing.is_empty() {
            missing.sort();
            let msg = missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            bail!(
                "--package-list: no usable apis/<name>/ under {} for: {} (each needs domain.yaml + mappings.yaml)",
                apis_root.display(),
                msg
            );
        }
    }

    if packed == 0 {
        bail!(
            "no API packages under {}: expected subdirs with domain.yaml and mappings.yaml{}",
            apis_root.display(),
            if allowed.is_some() {
                " (check --package-list)"
            } else {
                ""
            }
        );
    }

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
