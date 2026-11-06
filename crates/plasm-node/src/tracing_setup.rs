//! Process-wide diagnostics for the NAPI host. A pre-installed subscriber retains ownership.
use std::sync::{Once, OnceLock};
use tracing_subscriber::EnvFilter;

pub(crate) fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if [
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
        ]
        .iter()
        .any(|key| std::env::var(key).is_ok_and(|value| !value.trim().is_empty()))
        {
            static METRICS_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
            match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
            {
                Ok(runtime) => {
                    let runtime = METRICS_RUNTIME.get_or_init(|| runtime);
                    let _guard = runtime.enter();
                    if let Err(error) = plasm_agent_core::init_embedded_metrics() {
                        eprintln!("Plasm embedded metrics initialization failed: {error}");
                    }
                }
                Err(error) => eprintln!("Plasm metrics runtime initialization failed: {error}"),
            }
        }
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
