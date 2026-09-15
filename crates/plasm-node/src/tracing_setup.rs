//! Process-wide diagnostics for the NAPI host. A pre-installed subscriber retains ownership.
use std::sync::Once;
use tracing_subscriber::EnvFilter;

pub(crate) fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let filter = EnvFilter::builder()
            .with_default_directive(tracing_subscriber::filter::LevelFilter::WARN.into())
            .from_env_lossy();
        // Embedders may already own the global subscriber. Never replace it.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init();
    });
}
