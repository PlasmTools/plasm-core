//! View DAG preflight — schema stubs + inner CML compile gates (no HTTP).

use std::collections::BTreeMap;

use indexmap::IndexMap;
use plasm_core::{GetExpr, Predicate, QueryExpr, Value, CGS};

use crate::execution::preflight_compile_expr;
use crate::execution::ExecutionResult;
use crate::view_dag_run::run_view_dag_sync;
use crate::view_plan::{
    derive_view_get_scope, derive_view_query_scope, resolve_binding, ViewAmbientContext,
    ViewNodeFieldMap, ViewNodeRunner, ViewRunContext, ViewRunProof,
};
use crate::view_stub_rows::{stub_get_result, stub_query_result};
use crate::RuntimeError;

/// Preflight one view-backed query (derive scope from predicate, walk DAG with stubs).
pub fn preflight_view_query(
    view_name: &str,
    query: &QueryExpr,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
) -> Result<(), RuntimeError> {
    preflight_view_scoped_with_proof(
        view_name,
        derive_view_query_scope(view_name, query, cgs)?,
        cgs,
        ambient,
    )
    .map(|_| ())
}

/// Preflight one view-backed get (derive scope from reference key).
pub fn preflight_view_get(
    view_name: &str,
    get: &GetExpr,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
) -> Result<(), RuntimeError> {
    preflight_view_scoped_with_proof(
        view_name,
        derive_view_get_scope(view_name, get, cgs)?,
        cgs,
        ambient,
    )
    .map(|_| ())
}

/// Preflight a scoped view DAG and return structured proof (tests / debug hooks).
pub fn preflight_view_scoped_with_proof(
    view_name: &str,
    scope: IndexMap<String, Value>,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
) -> Result<ViewRunProof, RuntimeError> {
    let runner = PreflightViewNodeRunner { cgs, ambient };
    run_view_dag_sync(&runner, view_name, scope, cgs, ambient).map(|(proof, _)| proof)
}

pub(crate) struct PreflightViewNodeRunner<'a> {
    pub(crate) cgs: &'a CGS,
    pub(crate) ambient: &'a ViewAmbientContext,
}

impl ViewNodeRunner for PreflightViewNodeRunner<'_> {
    fn run_query_node(
        &self,
        ctx: &ViewRunContext<'_>,
        node: &plasm_core::schema::ViewNodeSpec,
        cap: &plasm_core::CapabilitySchema,
        pred: &Predicate,
        node_fields: &ViewNodeFieldMap,
    ) -> Result<ExecutionResult, RuntimeError> {
        let q = QueryExpr::filtered(cap.domain.as_str(), pred.clone());
        preflight_compile_expr(&plasm_core::Expr::Query(q), self.cgs, self.ambient)?;
        let mut bound_values = IndexMap::with_capacity(node.bind.len());
        for (param, bspec) in &node.bind {
            bound_values.insert(
                param.clone(),
                resolve_binding(bspec, ctx.scope, node_fields)?,
            );
        }
        stub_query_result(cap, self.cgs, &bound_values)
    }

    fn run_get_node(
        &self,
        _ctx: &ViewRunContext<'_>,
        _node: &plasm_core::schema::ViewNodeSpec,
        cap: &plasm_core::CapabilitySchema,
        get: &GetExpr,
        bound: &BTreeMap<String, String>,
    ) -> Result<ExecutionResult, RuntimeError> {
        preflight_compile_expr(&plasm_core::Expr::Get(get.clone()), self.cgs, self.ambient)?;
        stub_get_result(cap, self.cgs, bound)
    }
}
