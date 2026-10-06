//! Environment-driven configuration (dev-friendly defaults).

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TraceSinkConfigError {
    #[error("PLASM_TRACE_SINK_CATALOG_URL is required")]
    CatalogUrlRequired,
    #[error("catalog connection URL must be non-empty")]
    EmptyCatalogUrl,
    #[error("Iceberg SqlCatalog metadata requires Postgres, not SQLite")]
    SqliteCatalogUnsupported,
    #[error("catalog URL must use postgres:// or postgresql://")]
    UnsupportedCatalogScheme,
    #[error("S3 warehouse URI must start with s3:// or s3a://")]
    UnsupportedWarehouseScheme,
    #[error("S3 warehouse URI must include a bucket name")]
    MissingWarehouseBucket,
    #[error("PLASM_TRACE_SINK_WAREHOUSE_URL is invalid; filesystem path {ignored_path} is ignored while it is set")]
    InvalidWarehouseUrl {
        #[source]
        source: S3WarehouseUriError,
        ignored_path: PathBuf,
    },
    #[error("PLASM_TRACE_SINK_ICEBERG={value} is not supported; Iceberg is required")]
    IcebergDisabled { value: String },
}

/// Validated non-empty SqlCatalog JDBC URL (**Postgres only**; Iceberg metadata does not use SQLite).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogConnectionString(String);

impl CatalogConnectionString {
    fn new(raw: String) -> Result<Self, TraceSinkConfigError> {
        if raw.trim().is_empty() {
            return Err(TraceSinkConfigError::EmptyCatalogUrl);
        }
        let t = raw.trim();
        let lower = t.to_ascii_lowercase();
        if lower.starts_with("sqlite:") {
            return Err(TraceSinkConfigError::SqliteCatalogUnsupported);
        }
        if !(lower.starts_with("postgres://") || lower.starts_with("postgresql://")) {
            return Err(TraceSinkConfigError::UnsupportedCatalogScheme);
        }
        Ok(Self(raw))
    }

    /// Resolve from `PLASM_TRACE_SINK_CATALOG_URL` (`explicit`); **no default** sqlite file.
    pub fn resolve(_data_dir: &Path, explicit: Option<&str>) -> Result<Self, TraceSinkConfigError> {
        let Some(u) = explicit.map(str::trim).filter(|s| !s.is_empty()) else {
            return Err(TraceSinkConfigError::CatalogUrlRequired);
        };
        Self::new(u.to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Userinfo redaction for Postgres URLs (safe for structured logs).
    #[must_use]
    pub fn redacted_for_logs(&self) -> String {
        redact_jdbc_userinfo(&self.0)
    }
}

fn redact_jdbc_userinfo(url: &str) -> String {
    if let Some(at) = url.find('@') {
        if url.starts_with("postgresql://") || url.starts_with("postgres://") {
            return format!("postgresql://***:***{}", &url[at..]);
        }
    }
    url.to_string()
}

/// Validated Iceberg warehouse base for S3-compatible object storage (`s3://bucket` or `s3://bucket/prefix`).
///
/// Construct via [`S3WarehouseUri::parse`]. Used when resolving [`WarehouseLocation::S3`].
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct S3WarehouseUri(String);

#[derive(Debug, Error)]
pub enum S3WarehouseUriError {
    #[error("warehouse URI scheme is not S3-compatible")]
    UnsupportedScheme,
    #[error("warehouse URI has no bucket")]
    MissingBucket,
}

impl S3WarehouseUri {
    /// Normalize a raw URI to stable `s3://…` (accepts `s3a://` input).
    ///
    /// Errors are **not** tied to a specific environment variable; callers attach context
    /// (e.g. `PLASM_TRACE_SINK_WAREHOUSE_URL`) when appropriate.
    pub fn parse(raw: &str) -> Result<Self, S3WarehouseUriError> {
        let s = raw.trim();
        let host_and_path = s
            .strip_prefix("s3://")
            .or_else(|| s.strip_prefix("s3a://"))
            .ok_or(S3WarehouseUriError::UnsupportedScheme)?;
        let host_and_path = host_and_path.trim_end_matches('/');
        if host_and_path.is_empty() {
            return Err(S3WarehouseUriError::MissingBucket);
        }
        Ok(Self(format!("s3://{host_and_path}")))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Iceberg warehouse: local Parquet files or S3-compatible object storage (`s3://` / `s3a://`).
///
/// S3 mode uses [`iceberg_rust::object_store::ObjectStoreBuilder::s3`] (`AmazonS3Builder::from_env`).
/// Set `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and for Vultr (and similar) `AWS_ENDPOINT_URL`
/// / `AWS_REGION` per [`object_store`](https://docs.rs/object_store) S3 configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WarehouseLocation {
    Filesystem(PathBuf),
    S3 { base_url: String },
}

impl WarehouseLocation {
    #[must_use]
    pub fn summary_for_logs(&self) -> String {
        match self {
            Self::Filesystem(p) => p.display().to_string(),
            Self::S3 { base_url } => base_url.clone(),
        }
    }
}

/// Lawful bundle for [`crate::iceberg_writer::IcebergSink::connect`]: catalog + warehouse.
///
/// Build via [`Config::iceberg_connect_params`] or construct manually in tests, then pass
/// `&params` to [`IcebergSink::connect`](crate::iceberg_writer::IcebergSink::connect).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IcebergConnectParams {
    pub catalog: CatalogConnectionString,
    pub warehouse: WarehouseLocation,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: String,
    pub data_dir: PathBuf,
    /// Postgres SqlCatalog JDBC URL (`PLASM_TRACE_SINK_CATALOG_URL`); required for startup.
    pub catalog_url: Option<String>,
    /// Local Parquet root when **no** S3 warehouse URL is set ([`Self::warehouse_s3_url`] empty).
    ///
    /// **Precedence:** If `warehouse_s3_url` is non-empty after trim, [`IcebergConnectParams`]
    /// uses [`WarehouseLocation::S3`] and **`warehouse_fs_path` is ignored** for Iceberg data files
    /// (no warning). Otherwise [`WarehouseLocation::Filesystem`] uses this path.
    pub warehouse_fs_path: PathBuf,
    /// When non-empty, Iceberg Parquet uses S3-compatible storage ([`WarehouseLocation::S3`]).
    ///
    /// Takes precedence over [`Self::warehouse_fs_path`]. Set `PLASM_TRACE_SINK_WAREHOUSE_URL`
    /// at runtime; see [`S3WarehouseUri::parse`] for accepted forms.
    pub warehouse_s3_url: Option<String>,
    /// Hot `trace_segments` SQL cache TTL (`PLASM_TRACE_SINK_SEGMENT_PROJECTION_TTL_SECS`); `0` disables.
    pub segment_projection_ttl_secs: u64,
    /// Background purge interval for expired `trace_segments` rows.
    pub segment_projection_gc_interval_secs: u64,
}

impl Config {
    pub fn from_env() -> Self {
        let data_dir = std::env::var("PLASM_TRACE_SINK_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("var/plasm-trace-sink"));
        let warehouse_fs_path = std::env::var("PLASM_TRACE_SINK_WAREHOUSE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| data_dir.join("iceberg_warehouse"));
        let warehouse_s3_url = std::env::var("PLASM_TRACE_SINK_WAREHOUSE_URL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let catalog_url = std::env::var("PLASM_TRACE_SINK_CATALOG_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let segment_projection_ttl_secs =
            std::env::var("PLASM_TRACE_SINK_SEGMENT_PROJECTION_TTL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(604_800);
        let segment_projection_gc_interval_secs =
            std::env::var("PLASM_TRACE_SINK_SEGMENT_PROJECTION_GC_INTERVAL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .filter(|&n| n > 0)
                .unwrap_or(300);
        Self {
            listen: std::env::var("PLASM_TRACE_SINK_LISTEN")
                .unwrap_or_else(|_| "127.0.0.1:7070".to_string()),
            data_dir,
            catalog_url,
            warehouse_fs_path,
            warehouse_s3_url,
            segment_projection_ttl_secs,
            segment_projection_gc_interval_secs,
        }
    }

    /// Resolved catalog URL + warehouse for Iceberg startup.
    pub fn iceberg_connect_params(&self) -> Result<IcebergConnectParams, TraceSinkConfigError> {
        let warehouse = if let Some(raw) = &self.warehouse_s3_url {
            let uri = S3WarehouseUri::parse(raw).map_err(|source| {
                TraceSinkConfigError::InvalidWarehouseUrl {
                    source,
                    ignored_path: self.warehouse_fs_path.clone(),
                }
            })?;
            WarehouseLocation::S3 {
                base_url: uri.as_str().to_string(),
            }
        } else {
            WarehouseLocation::Filesystem(self.warehouse_fs_path.clone())
        };
        Ok(IcebergConnectParams {
            catalog: CatalogConnectionString::resolve(&self.data_dir, self.catalog_url.as_deref())?,
            warehouse,
        })
    }

    /// Resolved SqlCatalog JDBC URL string (same as [`IcebergConnectParams::catalog`]).
    ///
    /// Returns an error if [`Self::iceberg_connect_params`] fails (e.g. missing catalog URL, invalid warehouse URL).
    pub fn resolved_catalog_url(&self) -> Result<String, TraceSinkConfigError> {
        Ok(self.iceberg_connect_params()?.catalog.as_str().to_string())
    }

    /// Rejects deprecated in-memory-only mode (`PLASM_TRACE_SINK_ICEBERG=0`).
    pub fn ensure_iceberg_not_disabled() -> Result<(), TraceSinkConfigError> {
        match std::env::var("PLASM_TRACE_SINK_ICEBERG") {
            Ok(v)
                if v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("no") =>
            {
                return Err(TraceSinkConfigError::IcebergDisabled { value: v });
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{CatalogConnectionString, Config, S3WarehouseUri};

    #[test]
    fn catalog_requires_explicit_postgres_url() {
        assert!(CatalogConnectionString::resolve(Path::new("."), None).is_err());
    }

    #[test]
    fn catalog_rejects_sqlite_url() {
        assert!(CatalogConnectionString::resolve(Path::new("."), Some("sqlite://x/y.db")).is_err());
    }

    #[test]
    fn s3_warehouse_uri_accepts_bucket_only() {
        assert_eq!(
            S3WarehouseUri::parse("s3://my-bucket").unwrap().as_str(),
            "s3://my-bucket"
        );
    }

    #[test]
    fn s3_warehouse_uri_normalizes_prefix_and_scheme() {
        assert_eq!(
            S3WarehouseUri::parse("s3a://b/prefix//").unwrap().as_str(),
            "s3://b/prefix"
        );
    }

    #[test]
    fn s3_warehouse_uri_rejects_non_s3() {
        assert!(S3WarehouseUri::parse("https://x").is_err());
    }

    #[test]
    fn segment_projection_ttl_defaults_to_seven_days() {
        let cfg = Config {
            listen: "127.0.0.1:7070".into(),
            data_dir: PathBuf::from("var/plasm-trace-sink"),
            catalog_url: None,
            warehouse_fs_path: PathBuf::from("var/w"),
            warehouse_s3_url: None,
            segment_projection_ttl_secs: 604_800,
            segment_projection_gc_interval_secs: 300,
        };
        assert_eq!(cfg.segment_projection_ttl_secs, 604_800);
    }
}
