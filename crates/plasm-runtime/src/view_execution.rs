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
    async fn run_traversal_node(
        &mut self,
        ctx: &ViewRunContext<'_>,
        node: &ViewNodeSpec,
        source: &ExecutionResult,
    ) -> Result<ExecutionResult, RuntimeError> {
        let traverse = node.traverse.as_ref().expect("validated traversal node");
        let entity = ctx
            .cgs
            .view_node_entity(
                ctx.cgs.views.get(ctx.view_name).expect("loaded view"),
                &traverse.node,
            )
            .map_err(|message| RuntimeError::ConfigurationError { message })?;
        let chain = plasm_core::ChainExpr::auto_get(
            plasm_core::Expr::Query(QueryExpr::all(entity.as_str())),
            &traverse.relation,
        );
        // Source I/O was accounted by its own DAG node. Reuse its rows and proof,
        // while accounting only additional relation work on this node.
        let mut input = source.clone();
        input.stats = Default::default();
        input.request_fingerprints.clear();
        input.operations = Default::default();
        let source_def = self
            .cgs
            .get_entity(entity.as_str())
            .expect("validated source");
        let relation = &source_def.relations[traverse.relation.as_str()];
        if matches!(
            relation.materialize,
            Some(plasm_core::RelationMaterialization::FromParentGet { .. })
        ) {
            // Inner list queries deliberately skip automatic hydration. A GET-embedded
            // relation therefore requires observing each parent before extracting refs.
            for row in &mut input.entities {
                if !row.relations.contains_key(traverse.relation.as_str()) {
                    let observed = self
                        .engine
                        .execute_get_for_view_dag(
                            &GetExpr::from_ref(row.reference.clone()),
                            self.cgs,
                            self.cache,
                            self.mode,
                            Some(traverse.relation.as_str()),
                        )
                        .await?;
                    input.stats.network_requests += observed.stats.network_requests;
                    input.stats.cache_hits += observed.stats.cache_hits;
                    input
                        .request_fingerprints
                        .extend(observed.request_fingerprints);
                    input.coverage = crate::execution::ResultCoverage::combine_all([
                        input.coverage,
                        observed.coverage,
                    ]);
                    *row = observed.entities.into_iter().next().ok_or_else(|| {
                        RuntimeError::ConfigurationError {
                            message: format!("parent GET returned no row for {}", row.reference),
                        }
                    })?;
                    if row.reference.entity_type != entity {
                        return Err(RuntimeError::ConfigurationError {
                            message: "traversal parent returned a different entity type".into(),
                        });
                    }
                    if !row.relations.contains_key(traverse.relation.as_str()) {
                        return Err(RuntimeError::ConfigurationError {
                            message: format!(
                                "parent GET did not establish relation {}.{}",
                                row.reference, traverse.relation
                            ),
                        });
                    }
                }
            }
        }
        let opts = crate::execution::ExecuteOptions {
            http_base_url_override: self.ambient.transport_origin.clone(),
            ..Default::default()
        };
        let mut result = self
            .engine
            .execute_chain_materialized(
                &chain,
                input,
                self.cgs,
                self.cache,
                self.mode,
                StreamConsumeOpts {
                    fetch_all: true,
                    ..Default::default()
                },
                opts,
            )
            .await?;
        result.coverage =
            crate::execution::ResultCoverage::combine_all([source.coverage, result.coverage]);
        Ok(result)
    }

    async fn run_query_node(
        &mut self,
        _ctx: &ViewRunContext<'_>,
        _node: &ViewNodeSpec,
        cap: &CapabilitySchema,
        pred: &Predicate,
        _node_fields: &ViewNodeFieldMap,
    ) -> Result<ExecutionResult, RuntimeError> {
        let mut q = QueryExpr::filtered(cap.domain.as_str(), pred.clone());
        // Inner view DAG queries must not hydrate via an outer view-backed get on the
        // same entity (list → get → list recursion).
        q.capability_name = Some(cap.name.clone());
        q.hydrate = Some(false);
        self.engine
            .execute_query(
                &q,
                self.cgs,
                self.cache,
                self.mode,
                StreamConsumeOpts {
                    fetch_all: true,
                    ..Default::default()
                },
                self.ambient,
            )
            .await
    }

    async fn run_get_node(
        &mut self,
        _ctx: &ViewRunContext<'_>,
        _node: &ViewNodeSpec,
        cap: &CapabilitySchema,
        get: &GetExpr,
        _bound: &BTreeMap<String, String>,
    ) -> Result<ExecutionResult, RuntimeError> {
        self.engine
            .execute_get_for_view_dag(
                &get.clone().with_capability(cap.name.clone()),
                self.cgs,
                self.cache,
                self.mode,
                None,
            )
            .await
    }

    async fn run_create_node(
        &mut self,
        _ctx: &ViewRunContext<'_>,
        _node: &ViewNodeSpec,
        _cap: &CapabilitySchema,
        create: &plasm_core::CreateExpr,
    ) -> Result<ExecutionResult, RuntimeError> {
        self.engine
            .execute_create(create, self.cgs, self.cache, self.mode)
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
    let result = run_view_dag_async(&mut runner, view_name, scope, cgs, ambient).await?;
    // View rows participate in the same identity-bound graph as transport rows.
    // Publish only after the entire composition has established its output.
    for row in &result.entities {
        cache.publish_fresh_row(row.clone())?;
    }
    Ok(result)
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
