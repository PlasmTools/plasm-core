//! In-crate test fixtures (unit + Shuttle); not part of the public API.

pub mod execution_fixtures;
pub mod exposure_replay_fixtures;
pub mod graph_fixtures;
pub mod operation_fixtures;
pub mod session_fixtures;

/// Exercise host paths on a fixed normal worker stack, independent of RUST_MIN_STACK.
/// Overflow aborts the test process, so this is a stack-budget regression gate.
pub fn run_on_worker_stack<F>(f: F)
where
    F: FnOnce() + Send + 'static,
{
    std::thread::Builder::new()
        .name("plasm-worker-stack".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(f)
        .expect("spawn stack-budget worker")
        .join()
        .expect("stack-budget worker panicked");
}

pub fn block_on_worker_stack<F, Fut>(f: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    run_on_worker_stack(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("stack-budget runtime");
        rt.block_on(f());
    });
}
