//! Optional on-disk trace archive for OSS/self-host (`PLASM_TRACE_ARCHIVE_DIR`).
//! Layout: `traces/{tenant_id}/{trace_id}/summary.json` + `records.ndjson` (one JSON per line).

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use tokio::io::AsyncBufReadExt;
use uuid::Uuid;

use crate::trace_hub::{TraceDetailDto, TraceListStatus, TraceSummaryDto};

/// Local filesystem trace history (read + write) when `PLASM_TRACE_ARCHIVE_DIR` is set.
#[derive(Debug, Clone)]
pub struct LocalTraceArchive {
    root: PathBuf,
}

type ArcLocal = Arc<LocalTraceArchive>;

fn safe_fs_segment(s: &str) -> Result<&str, std::io::Error> {
    if s.is_empty() || s.contains("..") || s.contains('/') || s.contains('\\') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid path segment in trace archive key",
        ));
    }
    Ok(s)
}

impl LocalTraceArchive {
    pub fn new(root: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// `Some(archive)` when `PLASM_TRACE_ARCHIVE_DIR` is a non-empty path.
    pub fn from_env() -> std::io::Result<Option<ArcLocal>> {
        Self::from_env_or_oss_default(false)
    }

    /// Like [`Self::from_env`], or when `oss_local_filesystem_defaults` and OSS local persistence are enabled,
    /// use [`crate::oss_local_state::resolve_local_state_root`] as the archive root (`{root}/traces/...` layout).
    pub fn from_env_or_oss_default(
        oss_local_filesystem_defaults: bool,
    ) -> std::io::Result<Option<ArcLocal>> {
        match std::env::var("PLASM_TRACE_ARCHIVE_DIR") {
            Ok(s) if !s.trim().is_empty() => Ok(Some(Arc::new(Self::new(s.trim().into())?))),
            _ => {
                if oss_local_filesystem_defaults
                    && crate::oss_local_state::oss_local_persistence_enabled()
                {
                    if let Some(root) = crate::oss_local_state::resolve_local_state_root() {
                        return Ok(Some(Arc::new(Self::new(root)?)));
                    }
                }
                Ok(None)
            }
        }
    }

    fn trace_dir(&self, tenant_id: &str, trace_id: Uuid) -> Result<PathBuf, std::io::Error> {
        let t = safe_fs_segment(tenant_id)?;
        Ok(self
            .root
            .join("traces")
            .join(t)
            .join(trace_id.hyphenated().to_string()))
    }

    /// Best-effort persist a completed trace for later list/detail.
    pub async fn persist_trace(&self, detail: &TraceDetailDto) -> std::io::Result<()> {
        let trace_id = Uuid::parse_str(&detail.summary.trace_id).map_err(|e| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, format!("trace_id: {e}"))
        })?;
        let dir = self.trace_dir(&detail.summary.tenant_id, trace_id)?;
        tokio::fs::create_dir_all(&dir).await?;
        let summary = serde_json::to_string_pretty(&detail.summary)?;
        tokio::fs::write(dir.join("summary.json"), summary).await?;
        let mut body = String::new();
        for r in &detail.records {
            body.push_str(&serde_json::to_string(r)?);
            body.push('\n');
        }
        tokio::fs::write(dir.join("records.ndjson"), body).await?;
        Ok(())
    }

    /// List completed traces for a tenant (newest first by `started_at_ms` in `summary.json`).
    pub async fn list_for_tenant(
        &self,
        tenant_id: &str,
        project_slug: Option<&str>,
        offset: usize,
        limit: usize,
        status: TraceListStatus,
    ) -> std::io::Result<Vec<TraceSummaryDto>> {
        if status == TraceListStatus::Live {
            return Ok(Vec::new());
        }
        let t = safe_fs_segment(tenant_id)?;
        let dir = self.root.join("traces").join(t);
        let mut rd = match tokio::fs::read_dir(&dir).await {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut summaries: Vec<(u64, TraceSummaryDto)> = Vec::new();
        while let Some(ent) = rd.next_entry().await? {
            let path = ent.path();
            if !path.is_dir() {
                continue;
            }
            let file = path.join("summary.json");
            let data = match tokio::fs::read_to_string(&file).await {
                Ok(s) => s,
                Err(_) => continue,
            };
            if let Ok(fs) = serde_json::from_str::<TraceSummaryFile>(&data) {
                let dto = fs.into_dto();
                if let Some(want) = project_slug.filter(|p| !p.is_empty()) {
                    if dto.project_slug != want {
                        continue;
                    }
                } else if !(dto.project_slug == "main" || dto.project_slug.is_empty()) {
                    continue;
                }
                if dto.tenant_id != tenant_id {
                    continue;
                }
                summaries.push((dto.started_at_ms, dto));
            }
        }
        summaries.sort_by(|a, b| b.0.cmp(&a.0));
        Ok(summaries
            .into_iter()
            .map(|(_, s)| s)
            .skip(offset)
            .take(limit)
            .collect())
    }

    pub async fn get_detail(
        &self,
        tenant_id: &str,
        trace_id: Uuid,
    ) -> std::io::Result<Option<TraceDetailDto>> {
        let sum_path = self.trace_dir(tenant_id, trace_id)?.join("summary.json");
        let data = match tokio::fs::read_to_string(&sum_path).await {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let file: TraceSummaryFile = serde_json::from_str(&data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let summary = file.into_dto();
        if summary.tenant_id != tenant_id || summary.trace_id != trace_id.hyphenated().to_string() {
            return Ok(None);
        }
        let dir = self.trace_dir(tenant_id, trace_id)?;
        let rec_path = dir.join("records.ndjson");
        let mut records: Vec<serde_json::Value> = Vec::new();
        if let Ok(f) = tokio::fs::File::open(&rec_path).await {
            let reader = tokio::io::BufReader::new(f);
            let mut lines = reader.lines();
            while let Some(line) = lines.next_line().await? {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str(&line) {
                    records.push(v);
                }
            }
        }
        Ok(Some(TraceDetailDto { summary, records }))
    }
}

#[derive(Deserialize)]
struct TraceSummaryFile {
    trace_id: String,
    mcp_session_id: String,
    logical_session_id: Option<String>,
    status: String,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    project_slug: String,
    tenant_id: String,
    mcp_config: Option<crate::trace_hub::McpConfigRef>,
    totals: plasm_trace::TraceTotals,
}

impl TraceSummaryFile {
    fn into_dto(self) -> TraceSummaryDto {
        let st: &'static str = if self.status == "live" {
            "live"
        } else {
            "completed"
        };
        TraceSummaryDto {
            trace_id: self.trace_id,
            mcp_session_id: self.mcp_session_id,
            logical_session_id: self.logical_session_id,
            status: st,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
            project_slug: self.project_slug,
            tenant_id: self.tenant_id,
            mcp_config: self.mcp_config,
            totals: self.totals,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_trace_round_trip() {
        let root = std::env::temp_dir().join("plasm_trace_arch_test");
        let _ = std::fs::remove_dir_all(&root);
        let arch = LocalTraceArchive::new(root.clone()).unwrap();
        let d = TraceDetailDto {
            summary: TraceSummaryDto {
                trace_id: Uuid::new_v4().to_string(),
                mcp_session_id: "mcp".into(),
                logical_session_id: None,
                status: "completed",
                started_at_ms: 10,
                ended_at_ms: Some(20),
                project_slug: "main".into(),
                tenant_id: "t1".into(),
                mcp_config: None,
                totals: plasm_trace::TraceTotals::default(),
            },
            records: vec![serde_json::json!({"a":1})],
        };
        let tid = Uuid::parse_str(&d.summary.trace_id).unwrap();
        arch.persist_trace(&d).await.unwrap();
        let l = arch
            .list_for_tenant("t1", None, 0, 10, TraceListStatus::All)
            .await
            .unwrap();
        assert_eq!(l.len(), 1);
        let g = arch.get_detail("t1", tid).await.unwrap().expect("detail");
        assert_eq!(g.summary.trace_id, d.summary.trace_id);
        assert_eq!(g.records.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }
}
