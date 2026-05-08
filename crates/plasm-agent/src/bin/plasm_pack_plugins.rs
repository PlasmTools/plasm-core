//! Build one `plasm-plugin-stub` cdylib per `apis/<name>/` tree for `--plugin-dir` runtime loading.
//!
//! Usage (from repo root):
//!   cargo run -p plasm-agent --bin plasm-pack-plugins -- --apis-root apis --output-dir target/plasm-plugins
//! Docker release builds add `--package-list deploy/packaged-apis.txt` to pack only whitelisted APIs.
//!
//! After pinning `CGS::entry_id` to the directory name, each graph is run through full `CGS::validate`
//! (expression-teaching surface, auth, …) so regressions fail at pack time in CI, not only at runtime load.

use anyhow::{bail, Context, Result};
use clap::Parser;
use plasm_compile::validate_cgs_capability_templates;
use plasm_core::loader::load_schema;
use plasm_core::schema::CGS;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

#[derive(Parser, Debug)]
#[command(name = "plasm-pack-plugins")]
struct Args {
    /// Root directory whose subdirs contain `domain.yaml` + `mappings.yaml` (e.g. repo `apis/`).
    #[arg(long, default_value = "apis")]
    apis_root: PathBuf,

    /// Directory to receive copied `libplasm_plugin_<entry_id>_v<version>_<hash>.<ext>` artifacts.
    #[arg(long, default_value = "target/plasm-plugins")]
    output_dir: PathBuf,

    /// `cargo build --release` for `plasm-plugin-stub` (use for production images).
    #[arg(long, action = clap::ArgAction::SetTrue)]
    release: bool,

    /// Cargo workspace root (contains root `Cargo.toml`).
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    /// Rebuild every plugin cdylib even when an up-to-date packed artifact already exists.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    force: bool,

    /// When set, each `cargo build` for `plasm-plugin-stub` uses this `--target` (artifacts under
    /// `target/<triple>/release/`). Used in Docker cross-builds: the pack driver runs on the host
    /// triple while plugin cdylibs are built for the image triple.
    #[arg(long)]
    cargo_target: Option<String>,

    /// Only pack APIs listed in this file (one `apis/<name>/` directory name per line; `#` starts a
    /// comment; blank lines ignored). When omitted, every subdirectory of `--apis-root` with
    /// `domain.yaml` + `mappings.yaml` is packed (local dev default). Docker release builds pass
    /// `deploy/packaged-apis.txt`.
    #[arg(long)]
    package_list: Option<PathBuf>,
}

fn stub_artifact_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "libplasm_plugin_stub.dylib"
    } else if cfg!(target_os = "windows") {
        "plasm_plugin_stub.dll"
    } else {
        "libplasm_plugin_stub.so"
    }
}

fn packed_name(entry_id: &str, version: u64, cgs_hash_hex: &str) -> String {
    let short_hash = cgs_hash_hex.chars().take(12).collect::<String>();
    if cfg!(target_os = "macos") {
        format!("libplasm_plugin_{entry_id}_v{version}_{short_hash}.dylib")
    } else if cfg!(target_os = "windows") {
        format!("plasm_plugin_{entry_id}_v{version}_{short_hash}.dll")
    } else {
        format!("libplasm_plugin_{entry_id}_v{version}_{short_hash}.so")
    }
}

fn packed_file_version_for_entry(file_name: &str, entry_id: &str) -> Option<u64> {
    let prefixes = [
        format!("libplasm_plugin_{entry_id}_v"),
        format!("plasm_plugin_{entry_id}_v"),
    ];
    for prefix in prefixes {
        if let Some(rest) = file_name.strip_prefix(&prefix) {
            let ver = rest.split('_').next()?;
            return ver.parse::<u64>().ok();
        }
    }
    None
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
            "plasm-pack-plugins: prune old `{}` artifact {}",
            entry_id,
            old.path.display()
        );
        fs::remove_file(&old.path).with_context(|| format!("remove {}", old.path.display()))?;
    }
    Ok(())
}

fn prepare_cgs_for_plugin(api_dir: &Path, entry_id: &str) -> Result<CGS> {
    let mut cgs = load_schema(api_dir)
        .map_err(|e| anyhow::anyhow!("load_schema {}: {e}", api_dir.display()))?;
    validate_cgs_capability_templates(&cgs)
        .map_err(|e| anyhow::anyhow!("validate {entry_id}: {e}"))?;

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

    // `load_schema` validated before `entry_id` was pinned (YAML often omits it). Runtime plugins and
    // teaching-bundle checks use the packed registry id — re-validate so CI `plasm-pack-plugins` fails
    // on expression-surface / coverage regressions instead of at deploy.
    cgs.validate()
        .map_err(|e| anyhow::anyhow!("CGS validate {entry_id} (after entry_id pin): {e}"))?;

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

fn write_interchange_yaml(cgs: &CGS, path: &Path) -> Result<()> {
    let s = serde_yaml::to_string(cgs).context("serde_yaml::to_string CGS")?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, s).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Fingerprint native code that links into `plasm-plugin-stub` cdylibs (path deps + lockfile).
fn plugin_native_toolchain_fingerprint(workspace: &Path) -> Result<String> {
    let mut h = Sha256::new();
    let lock = workspace.join("Cargo.lock");
    if lock.is_file() {
        h.update(b"Cargo.lock\0");
        h.update(fs::read(&lock).with_context(|| format!("read {}", lock.display()))?);
    }
    let root_manifest = workspace.join("Cargo.toml");
    if root_manifest.is_file() {
        h.update(b"root Cargo.toml\0");
        h.update(
            fs::read(&root_manifest)
                .with_context(|| format!("read {}", root_manifest.display()))?,
        );
    }
    for rel in [
        "plasm-plugin-stub",
        "plasm-plugin-abi",
        "plasm-compile",
        "plasm-core",
        "plasm-cml",
    ] {
        let dir = workspace.join("crates").join(rel);
        h.update(rel.as_bytes());
        h.update(b"\0");
        hash_crate_sources_for_fingerprint(&dir, &mut h)
            .with_context(|| format!("fingerprint crate {rel}"))?;
    }
    Ok(hex::encode(h.finalize()))
}

fn hash_crate_sources_for_fingerprint(dir: &Path, h: &mut Sha256) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut paths = Vec::<PathBuf>::new();
    collect_fingerprint_source_paths(dir, &mut paths)?;
    paths.sort();
    for p in paths {
        h.update(p.to_string_lossy().as_bytes());
        h.update(b"\0");
        let bytes = fs::read(&p).with_context(|| format!("read {}", p.display()))?;
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(&bytes);
    }
    Ok(())
}

fn collect_fingerprint_source_paths(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for ent in fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let ent = ent?;
        let p = ent.path();
        let name = ent.file_name();
        let name_s = name.to_string_lossy();
        if name_s == "target" || name_s.starts_with('.') {
            continue;
        }
        let ty = ent
            .file_type()
            .with_context(|| format!("file_type {}", p.display()))?;
        if ty.is_dir() {
            collect_fingerprint_source_paths(&p, out)?;
        } else if ty.is_file() {
            let ext = p.extension().and_then(OsStr::to_str);
            let is_toml = p.file_name() == Some(OsStr::new("Cargo.toml"));
            let is_build_rs = p.file_name() == Some(OsStr::new("build.rs"));
            if ext == Some("rs") || is_toml || is_build_rs {
                out.push(p);
            }
        }
    }
    Ok(())
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

    let profile = if args.release { "release" } else { "debug" };
    let target_dir = match &args.cargo_target {
        Some(triple) => workspace.join("target").join(triple).join(profile),
        None => workspace.join("target").join(profile),
    };
    let stub_src_name = stub_artifact_name();
    let built_stub = target_dir.join(stub_src_name);

    let toolchain_fp = plugin_native_toolchain_fingerprint(&workspace)?;

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
            "plasm-pack-plugins: package list enabled ({} entr{})",
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

        let cgs = prepare_cgs_for_plugin(&path, name)?;
        let cgs_hash = cgs.catalog_cgs_hash_hex();
        let dest = out_dir.join(packed_name(name, cgs.version, &cgs_hash));
        let stamp_path = cache_dir.join(format!("{name}.stamp"));
        let cargo_target_line = args.cargo_target.as_deref().unwrap_or("");
        let stamp_body = format!("{cgs_hash}\n{toolchain_fp}\n{profile}\n{cargo_target_line}\n");

        if !args.force
            && dest.is_file()
            && fs::read_to_string(&stamp_path).ok().as_deref() == Some(stamp_body.as_str())
        {
            eprintln!("plasm-pack-plugins: skip `{name}` (artifact up to date)");
            enforce_entry_retention(&out_dir, name, 1, &dest)?;
            skipped += 1;
            packed += 1;
            continue;
        }

        eprintln!("plasm-pack-plugins: packing `{name}` …");
        let tmp_yaml = out_dir.join(format!("_{name}.cgs.pack.yaml"));
        write_interchange_yaml(&cgs, &tmp_yaml)?;

        let mut cmd = Command::new("cargo");
        cmd.current_dir(&workspace)
            .env("PLASM_EMBEDDED_CGS", &tmp_yaml)
            .env("PLASM_EMBEDDED_CGS_API_DIR", &path)
            .arg("build")
            .arg("-p")
            .arg("plasm-plugin-stub");
        if args.release {
            cmd.arg("--release");
        }
        if let Some(triple) = args.cargo_target.as_deref() {
            cmd.arg("--target").arg(triple);
        }
        let status = cmd
            .status()
            .context("spawn cargo build -p plasm-plugin-stub")?;

        if !status.success() {
            bail!("cargo build plasm-plugin-stub failed for `{name}`: {status}");
        }

        if !built_stub.is_file() {
            bail!(
                "expected stub artifact missing after build: {}",
                built_stub.display()
            );
        }

        fs::copy(&built_stub, &dest)
            .with_context(|| format!("copy {} -> {}", built_stub.display(), dest.display()))?;
        let _ = fs::remove_file(&tmp_yaml);
        fs::write(&stamp_path, &stamp_body)
            .with_context(|| format!("write stamp {}", stamp_path.display()))?;

        // Sanity: reload metadata would match — optional hash log
        eprintln!(
            "plasm-pack-plugins: wrote {} (catalog hash {})",
            dest.display(),
            cgs_hash
        );
        enforce_entry_retention(&out_dir, name, 1, &dest)?;
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
            "plasm-pack-plugins: packed {packed} plugin(s) into {} (reused {skipped} unchanged)",
            out_dir.display()
        );
    } else {
        eprintln!(
            "plasm-pack-plugins: packed {packed} plugin(s) into {}",
            out_dir.display()
        );
    }
    Ok(())
}
