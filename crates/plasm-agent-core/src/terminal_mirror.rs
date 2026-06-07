//! Append-only per-session mirror archive (`s/<id>/out/NNNN-kind/`).

use anyhow::{Context as _, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::resolved_plan_http::ResolvedPlanResponse;
use crate::terminal_state::{display_mirror_path, session_dir, session_out_dir};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorOpKind {
    Search,
    Context,
    Plan,
    Run,
}

impl MirrorOpKind {
    fn dir_suffix(self) -> &'static str {
        match self {
            MirrorOpKind::Search => "search",
            MirrorOpKind::Context => "context",
            MirrorOpKind::Plan => "plan",
            MirrorOpKind::Run => "run",
        }
    }
}

pub struct SessionMirror {
    session_id: String,
    next_seq: u64,
}

impl SessionMirror {
    pub fn open(session_id: &str) -> Result<Self> {
        let out_root = session_out_dir(session_id);
        std::fs::create_dir_all(&out_root)?;
        let seq_path = out_root.join(".seq");
        let next_seq = if seq_path.exists() {
            let raw = std::fs::read_to_string(&seq_path)
                .with_context(|| format!("read {}", seq_path.display()))?;
            raw.trim().parse::<u64>().unwrap_or(0)
        } else {
            0
        };
        Ok(Self {
            session_id: session_id.to_string(),
            next_seq,
        })
    }

    pub fn alloc_dir(&mut self, kind: MirrorOpKind) -> Result<PathBuf> {
        self.next_seq = self.next_seq.saturating_add(1);
        let dir_name = format!("{:04}-{}", self.next_seq, kind.dir_suffix());
        let dir = session_out_dir(&self.session_id).join(&dir_name);
        std::fs::create_dir_all(&dir)?;
        let seq_path = session_out_dir(&self.session_id).join(".seq");
        std::fs::write(&seq_path, self.next_seq.to_string())?;
        Ok(dir)
    }

    pub fn rel_dir_for_display(&self, dir: &Path) -> String {
        let session_root = session_dir(&self.session_id);
        dir.strip_prefix(&session_root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| dir.display().to_string())
    }

    pub fn update_latest_pointer(&self, rel_from_session: &str) -> Result<()> {
        let path = session_dir(&self.session_id).join("latest");
        std::fs::write(&path, format!("{rel_from_session}\n"))?;
        Ok(())
    }

    pub fn write_file(&self, dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = dir.join(name);
        std::fs::write(&path, bytes)?;
        Ok(path)
    }

    /// Write `{base}.json` and `{base}.txt` from a single HTTP/raw body.
    pub fn write_pair(
        &self,
        dir: &Path,
        base: &str,
        raw: &[u8],
        content_type_hint: Option<&str>,
    ) -> Result<(PathBuf, PathBuf)> {
        if let Ok(v) = serde_json::from_slice::<Value>(raw) {
            let json_path = dir.join(format!("{base}.json"));
            let pretty = serde_json::to_string_pretty(&v)?;
            std::fs::write(&json_path, &pretty)?;
            let txt = resolved_plan_text_from_value(&v).unwrap_or(pretty);
            let txt_path = dir.join(format!("{base}.txt"));
            std::fs::write(&txt_path, txt)?;
            return Ok((json_path, txt_path));
        }
        let text = String::from_utf8_lossy(raw).into_owned();
        let txt_path = dir.join(format!("{base}.txt"));
        std::fs::write(&txt_path, &text)?;
        let envelope = serde_json::json!({
            "content_type": content_type_hint.unwrap_or("text/plain"),
            "body": text,
        });
        let json_path = dir.join(format!("{base}.json"));
        std::fs::write(&json_path, serde_json::to_string_pretty(&envelope)?)?;
        Ok((json_path, txt_path))
    }

    pub fn write_artifact_pair(&self, dir: &Path, raw: &[u8]) -> Result<PathBuf> {
        let (_json_path, txt_path) =
            self.write_pair(dir, "artifact", raw, Some("application/json"))?;
        Ok(txt_path)
    }
}

pub fn mirror_eprintln(path: &Path) {
    eprintln!("mirror: {}", display_mirror_path(path));
}

fn resolved_plan_text_from_value(v: &Value) -> Option<String> {
    if let Ok(resp) = serde_json::from_value::<ResolvedPlanResponse>(v.clone()) {
        if let Some(md) = resp.run_markdown.filter(|s| !s.is_empty()) {
            return Some(md);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_state::test_env::with_plasm_workspace;
    use tempfile::TempDir;

    #[test]
    fn write_pair_json_extracts_run_markdown() {
        let tmp = TempDir::new().expect("tempdir");
        with_plasm_workspace(tmp.path(), || {
            let sid = "a1b2c3d4";
            let mirror = SessionMirror::open(sid).expect("open");
            let mut m = mirror;
            let dir = m.alloc_dir(MirrorOpKind::Run).expect("alloc");
            let body = br#"{"plan":true,"dry_run":false,"plan_dag":{},"run_markdown":"hello"}"#;
            let (_, txt) = m.write_pair(&dir, "body", body, None).expect("pair");
            let text = std::fs::read_to_string(txt).expect("read txt");
            assert_eq!(text, "hello");
        });
    }

    #[test]
    fn seq_monotonic_across_alloc() {
        let tmp = TempDir::new().expect("tempdir");
        with_plasm_workspace(tmp.path(), || {
            let sid = "deadbeef";
            let mut m = SessionMirror::open(sid).expect("open");
            let d1 = m.alloc_dir(MirrorOpKind::Search).expect("a1");
            let d2 = m.alloc_dir(MirrorOpKind::Context).expect("a2");
            assert!(d1.ends_with("0001-search"));
            assert!(d2.ends_with("0002-context"));
        });
    }

    #[test]
    fn display_mirror_path_relativizes() {
        let tmp = TempDir::new().expect("tempdir");
        with_plasm_workspace(tmp.path(), || {
            let p = session_dir("abc12345").join("out/0001-run/body.txt");
            let shown = crate::terminal_state::display_mirror_path(&p);
            assert!(shown.contains(".plasm/s/abc12345"));
        });
    }
}
