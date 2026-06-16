//! CGS `views:` execution — composed reads without dedicated HTTP mappings.
//!
//! Live HTTP path: async node I/O via [`LiveViewNodeRunner`], shared DAG orchestration in [`crate::view_dag_run`].

use std::collections::BTreeMap;

use indexmap::IndexMap;
use plasm_core::schema::ViewNodeSpec;
use plasm_core::{CapabilitySchema, GetExpr, Predicate, QueryExpr, CGS};

use crate::execution::{ExecutionEngine, ExecutionMode, ExecutionResult, StreamConsumeOpts};
use crate::materialization::SessionMaterialization;
use crate::view_dag_run::run_view_dag_async;
use crate::view_plan::{
    derive_view_get_scope, derive_view_query_scope, ViewAmbientContext, ViewNodeFieldMap,
    ViewNodeRunnerAsync, ViewRunContext,
};
use crate::RuntimeError;

struct LiveViewNodeRunner<'a> {
    engine: &'a ExecutionEngine,
    cache: &'a mut SessionMaterialization,
    cgs: &'a CGS,
    mode: ExecutionMode,
    ambient: &'a ViewAmbientContext,
}

#[async_trait::async_trait]
impl ViewNodeRunnerAsync for LiveViewNodeRunner<'_> {
    async fn run_query_node(
        &mut self,
        _ctx: &ViewRunContext<'_>,
        _node: &ViewNodeSpec,
        cap: &CapabilitySchema,
        pred: &Predicate,
        _node_fields: &ViewNodeFieldMap,
    ) -> Result<ExecutionResult, RuntimeError> {
        let q = QueryExpr::filtered(cap.domain.as_str(), pred.clone());
        self.engine
            .execute_query(
                &q,
                self.cgs,
                self.cache,
                self.mode,
                StreamConsumeOpts::default(),
                self.ambient,
            )
            .await
    }

    async fn run_get_node(
        &mut self,
        _ctx: &ViewRunContext<'_>,
        _node: &ViewNodeSpec,
        _cap: &CapabilitySchema,
        get: &GetExpr,
        _bound: &BTreeMap<String, String>,
    ) -> Result<ExecutionResult, RuntimeError> {
        self.engine
            .execute_get_for_view_dag(get, self.cgs, self.cache, self.mode)
            .await
    }
}

async fn execute_view_scoped(
    engine: &ExecutionEngine,
    view_name: &str,
    scope: IndexMap<String, plasm_core::Value>,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    ambient: &ViewAmbientContext,
) -> Result<ExecutionResult, RuntimeError> {
    let mut runner = LiveViewNodeRunner {
        engine,
        cache,
        cgs,
        mode,
        ambient,
    };
    run_view_dag_async(&mut runner, view_name, scope, cgs, ambient).await
}

/// Run a `views:` composition for an outer [`QueryExpr`] (must target the view entity).
pub(crate) async fn execute_view_query(
    engine: &ExecutionEngine,
    view_name: &str,
    query: &QueryExpr,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    ambient: &ViewAmbientContext,
) -> Result<ExecutionResult, RuntimeError> {
    let scope = derive_view_query_scope(view_name, query, cgs)?;
    execute_view_scoped(engine, view_name, scope, cgs, cache, mode, ambient).await
}

/// Run a `views:` composition for an outer [`GetExpr`] on the view entity.
pub(crate) async fn execute_view_get(
    engine: &ExecutionEngine,
    view_name: &str,
    get: &GetExpr,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    ambient: &ViewAmbientContext,
) -> Result<ExecutionResult, RuntimeError> {
    let scope = derive_view_get_scope(view_name, get, cgs)?;
    execute_view_scoped(engine, view_name, scope, cgs, cache, mode, ambient).await
}
