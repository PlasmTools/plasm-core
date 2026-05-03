//! Default local state directory for OSS `plasm-agent` (`~/.plasm/local` or `PLASM_LOCAL_STATE_DIR`).

use std::path::PathBuf;

/// When `false`, OSS binary skips implicit `~/.plasm/local` run artifacts and trace archive defaults.
pub fn oss_local_persistence_enabled() -> bool {
    !matches!(
        std::env::var("PLASM_OSS_LOCAL_PERSISTENCE").ok().as_deref(),
        Some("0") | Some("false") | Some("FALSE")
    )
}

/// Resolve the OSS local state root (parent of default `run-artifacts/` and trace archive `traces/`).
pub fn resolve_local_state_root() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PLASM_LOCAL_STATE_DIR") {
        let p = p.trim();
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join(".plasm").join("local"))
}
