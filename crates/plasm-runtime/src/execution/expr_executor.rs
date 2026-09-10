//! ExprExecutor trait for host dispatch.

use super::*;

pub trait ExprExecutor: Send + Sync {
    /// Same contract as [`ExecutionEngine::execute`].
    fn execute<'a>(
        &'a self,
        expr: &'a Expr,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    >;
}

impl ExprExecutor for ExecutionEngine {
    fn execute<'a>(
        &'a self,
        expr: &'a Expr,
        cgs: &'a CGS,
        mat: &'a mut SessionMaterialization,
        mode: Option<ExecutionMode>,
        consume: StreamConsumeOpts,
        opts: ExecuteOptions,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>,
    > {
        ExecutionEngine::execute(self, expr, cgs, mat, mode, consume, opts)
    }
}
