//! Shared sync/async view DAG walk (live + preflight + fixture).

use indexmap::IndexMap;
use plasm_compile::DecodedRelation;
use plasm_core::schema::{EntityDef, ViewDefinition, ViewNodeSpec, ViewOutputBinding};
use plasm_core::{Ref, WriteOutcome, CGS};

use crate::cache::{CachedEntity, EntityCompleteness};
use crate::execution::{current_timestamp, ExecutionResult, ExecutionSource, ExecutionStats};
use crate::view_plan::{
    build_view_row_reference, load_view_dag, node_fields_for_row, prepare_view_node,
    resolve_output_binding, resolve_view_relation_maps, view_node_should_run, PreparedViewNode,
    ViewAmbientContext, ViewNodeFieldMap, ViewNodeRunner, ViewNodeRunnerAsync, ViewRunContext,
    ViewRunProof,
};
use crate::RuntimeError;

/// Node walk product passed to view DAG finalization.
pub(crate) struct ViewDagWalkOutcome {
    pub scope: IndexMap<String, plasm_core::Value>,
    pub node_results: IndexMap<String, ExecutionResult>,
    pub write_outcomes: IndexMap<String, WriteOutcome>,
    pub stats: ExecutionStats,
    pub fingerprints: Vec<String>,
    pub any_live: bool,
}

type MaterializedViewOutputs = (
    IndexMap<String, plasm_core::Value>,
    Ref,
    IndexMap<String, DecodedRelation>,
);

fn materialize_view_row(
    view: &ViewDefinition,
    view_entity: &EntityDef,
    scope: &IndexMap<String, plasm_core::Value>,
    node_results: &IndexMap<String, ExecutionResult>,
    write_outcomes: &IndexMap<String, WriteOutcome>,
    cgs: &CGS,
) -> Result<MaterializedViewOutputs, RuntimeError> {
    let mut fields_plain: IndexMap<String, plasm_core::Value> = IndexMap::new();
    for (fname, binding) in &view.output {
        if matches!(binding, ViewOutputBinding::Computed { .. }) {
            continue;
        }
        let v = resolve_output_binding(binding, scope, node_results, write_outcomes)?;
        fields_plain.insert(fname.clone(), v);
    }
    for (fname, binding) in &view.output {
        let ViewOutputBinding::Computed { template } = binding else {
            continue;
        };
        let v =
            crate::view_template::render_view_computed_template(template, scope, &fields_plain)?;
        fields_plain.insert(fname.clone(), v);
    }

    let row_ref = build_view_row_reference(view_entity, &fields_plain)?;
    let relation_refs = resolve_view_relation_maps(view, node_results, cgs)?;
    Ok((fields_plain, row_ref, relation_refs))
}

fn execution_result_from_view_row(
    output_fields: IndexMap<String, plasm_core::Value>,
    row_ref: Ref,
    relation_refs: IndexMap<String, DecodedRelation>,
    stats: ExecutionStats,
    fingerprints: Vec<String>,
    any_live: bool,
) -> ExecutionResult {
    let cached = CachedEntity::from_decoded(
        row_ref,
        output_fields,
        relation_refs,
        current_timestamp(),
        EntityCompleteness::Complete,
    );
    ExecutionResult {
        entities: vec![cached],
        count: 1,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: if any_live {
            ExecutionSource::Live
        } else {
            ExecutionSource::Cache
        },
        stats,
        request_fingerprints: fingerprints,
    }
}

fn finalize_view_dag_execution(
    view: &ViewDefinition,
    view_entity: &EntityDef,
    walk: ViewDagWalkOutcome,
    cgs: &CGS,
) -> Result<ExecutionResult, RuntimeError> {
    let ViewDagWalkOutcome {
        scope,
        node_results,
        write_outcomes,
        stats,
        fingerprints,
        any_live,
    } = walk;
    let (output_fields, row_ref, relation_refs) = materialize_view_row(
        view,
        view_entity,
        &scope,
        &node_results,
        &write_outcomes,
        cgs,
    )?;
    Ok(execution_result_from_view_row(
        output_fields,
        row_ref,
        relation_refs,
        stats,
        fingerprints,
        any_live,
    ))
}

/// Proof + execution finalize for test/fixture walks.
///
/// [`ViewRunProof`] and [`ExecutionResult`] both need row identity and relations; we clone
/// `row_ref` and `relation_refs` once for the cached entity and move the originals into the
/// proof. Proof `output_fields` are derived from the entity (no second plain-field map).
/// Shared `Arc` would avoid those clones but is not worth it on this preflight-only path.
fn finalize_view_dag_with_proof(
    view: &ViewDefinition,
    view_entity: &EntityDef,
    walk: ViewDagWalkOutcome,
    cgs: &CGS,
) -> Result<(ViewRunProof, ExecutionResult), RuntimeError> {
    let ViewDagWalkOutcome {
        scope,
        node_results,
        write_outcomes,
        stats,
        fingerprints,
        any_live,
    } = walk;
    let (output_fields, row_ref, relation_refs) = materialize_view_row(
        view,
        view_entity,
        &scope,
        &node_results,
        &write_outcomes,
        cgs,
    )?;
    let execution = execution_result_from_view_row(
        output_fields,
        row_ref.clone(),
        relation_refs.clone(),
        stats,
        fingerprints,
        any_live,
    );
    let cached = &execution.entities[0];
    let proof = ViewRunProof {
        scope,
        node_results,
        output_fields: cached
            .fields
            .iter()
            .map(|(k, v)| (k.clone(), v.to_value()))
            .collect(),
        relation_refs,
        row_ref,
        stats: execution.stats.clone(),
        request_fingerprints: execution.request_fingerprints.clone(),
        any_live,
    };
    Ok((proof, execution))
}

struct LoadedViewDag<'a> {
    view: &'a ViewDefinition,
    view_entity: &'a EntityDef,
    scope: IndexMap<String, plasm_core::Value>,
    view_name: &'a str,
    cgs: &'a CGS,
    ambient: &'a ViewAmbientContext,
}

impl<'a> LoadedViewDag<'a> {
    fn load(
        view_name: &'a str,
        scope: IndexMap<String, plasm_core::Value>,
        cgs: &'a CGS,
        ambient: &'a ViewAmbientContext,
    ) -> Result<Self, RuntimeError> {
        let (view, view_entity, scope) = load_view_dag(view_name, scope, cgs, ambient)?;
        Ok(Self {
            view,
            view_entity,
            scope,
            view_name,
            cgs,
            ambient,
        })
    }

    fn run_ctx(&self) -> ViewRunContext<'_> {
        ViewRunContext {
            view_name: self.view_name,
            scope: &self.scope,
            cgs: self.cgs,
            ambient: self.ambient,
        }
    }

    fn prepare_node(
        &self,
        node: &ViewNodeSpec,
        node_fields: &ViewNodeFieldMap,
    ) -> Result<PreparedViewNode<'_>, RuntimeError> {
        prepare_view_node(
            node,
            &self.scope,
            node_fields,
            self.cgs,
            self.view.entity.as_str(),
        )
    }
}

struct ViewDagWalkState {
    node_results: IndexMap<String, ExecutionResult>,
    write_outcomes: IndexMap<String, WriteOutcome>,
    node_fields: ViewNodeFieldMap,
    stats: ExecutionStats,
    fingerprints: Vec<String>,
    any_live: bool,
}

impl ViewDagWalkState {
    fn new() -> Self {
        Self {
            node_results: IndexMap::new(),
            write_outcomes: IndexMap::new(),
            node_fields: ViewNodeFieldMap::new(),
            stats: ExecutionStats::default(),
            fingerprints: Vec::new(),
            any_live: false,
        }
    }

    fn record(&mut self, node_id: String, res: ExecutionResult, outcome: Option<WriteOutcome>) {
        crate::view_plan::absorb_node_stats(
            &mut self.stats,
            &mut self.fingerprints,
            &mut self.any_live,
            &res,
        );
        self.node_fields
            .insert(node_id.clone(), node_fields_for_row(res.entities.first()));
        self.node_results.insert(node_id.clone(), res);
        if let Some(o) = outcome {
            self.write_outcomes.insert(node_id, o);
        }
    }

    fn record_skipped(&mut self, node_id: String) {
        self.write_outcomes
            .insert(node_id.clone(), WriteOutcome::Skipped);
        self.node_results.insert(
            node_id,
            ExecutionResult {
                entities: vec![],
                count: 0,
                has_more: false,
                pagination_resume: None,
                paging_handle: None,
                source: ExecutionSource::Cache,
                stats: ExecutionStats::default(),
                request_fingerprints: Vec::new(),
            },
        );
    }

    fn into_outcome(self, scope: IndexMap<String, plasm_core::Value>) -> ViewDagWalkOutcome {
        ViewDagWalkOutcome {
            scope,
            node_results: self.node_results,
            write_outcomes: self.write_outcomes,
            stats: self.stats,
            fingerprints: self.fingerprints,
            any_live: self.any_live,
        }
    }
}

fn dispatch_prepared_sync<R: ViewNodeRunner>(
    runner: &R,
    run_ctx: &ViewRunContext<'_>,
    node: &ViewNodeSpec,
    prepared: PreparedViewNode<'_>,
    node_fields: &ViewNodeFieldMap,
) -> Result<ExecutionResult, RuntimeError> {
    match prepared {
        PreparedViewNode::Query { cap, pred } => {
            runner.run_query_node(run_ctx, node, cap, &pred, node_fields)
        }
        PreparedViewNode::Get { cap, get, bound } => {
            runner.run_get_node(run_ctx, node, cap, &get, &bound)
        }
        PreparedViewNode::Create { cap, create } => {
            runner.run_create_node(run_ctx, node, cap, &create)
        }
    }
}

async fn dispatch_prepared_async<R: ViewNodeRunnerAsync + ?Sized>(
    runner: &mut R,
    run_ctx: &ViewRunContext<'_>,
    node: &ViewNodeSpec,
    prepared: PreparedViewNode<'_>,
    node_fields: &ViewNodeFieldMap,
) -> Result<ExecutionResult, RuntimeError> {
    match prepared {
        PreparedViewNode::Query { cap, pred } => {
            runner
                .run_query_node(run_ctx, node, cap, &pred, node_fields)
                .await
        }
        PreparedViewNode::Get { cap, get, bound } => {
            runner.run_get_node(run_ctx, node, cap, &get, &bound).await
        }
        PreparedViewNode::Create { cap, create } => {
            runner.run_create_node(run_ctx, node, cap, &create).await
        }
    }
}

fn write_outcome_from_result(res: &ExecutionResult) -> WriteOutcome {
    res.entities
        .first()
        .and_then(|e| e.fields.get("outcome"))
        .and_then(|v| v.to_value().as_str().map(str::to_string))
        .map(|s| match s.as_str() {
            "reused" => WriteOutcome::Reused,
            "skipped" => WriteOutcome::Skipped,
            _ => WriteOutcome::Created,
        })
        .unwrap_or(WriteOutcome::Created)
}

fn walk_view_nodes_sync<R: ViewNodeRunner>(
    runner: &R,
    loaded: &LoadedViewDag<'_>,
    walk: &mut ViewDagWalkState,
) -> Result<(), RuntimeError> {
    let run_ctx = loaded.run_ctx();
    for node in &loaded.view.nodes {
        if !view_node_should_run(node.when.as_ref(), &walk.node_results) {
            walk.record_skipped(node.id.clone());
            continue;
        }
        let prepared = loaded.prepare_node(node, &walk.node_fields)?;
        let is_write = matches!(prepared, PreparedViewNode::Create { .. });
        let res = dispatch_prepared_sync(runner, &run_ctx, node, prepared, &walk.node_fields)?;
        let outcome = is_write.then(|| write_outcome_from_result(&res));
        walk.record(node.id.clone(), res, outcome);
    }
    Ok(())
}

async fn walk_view_nodes_async<R: ViewNodeRunnerAsync + ?Sized>(
    runner: &mut R,
    loaded: &LoadedViewDag<'_>,
    walk: &mut ViewDagWalkState,
) -> Result<(), RuntimeError> {
    let run_ctx = loaded.run_ctx();
    for node in &loaded.view.nodes {
        if !view_node_should_run(node.when.as_ref(), &walk.node_results) {
            walk.record_skipped(node.id.clone());
            continue;
        }
        let prepared = loaded.prepare_node(node, &walk.node_fields)?;
        let is_write = matches!(prepared, PreparedViewNode::Create { .. });
        let res =
            dispatch_prepared_async(runner, &run_ctx, node, prepared, &walk.node_fields).await?;
        let outcome = is_write.then(|| write_outcome_from_result(&res));
        walk.record(node.id.clone(), res, outcome);
    }
    Ok(())
}

/// Walk view nodes via sync `runner`, then materialize output / relations / row ref.
pub(crate) fn run_view_dag_sync<R: ViewNodeRunner>(
    runner: &R,
    view_name: &str,
    scope: IndexMap<String, plasm_core::Value>,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
) -> Result<(ViewRunProof, ExecutionResult), RuntimeError> {
    let loaded = LoadedViewDag::load(view_name, scope, cgs, ambient)?;
    let mut walk = ViewDagWalkState::new();
    walk_view_nodes_sync(runner, &loaded, &mut walk)?;

    finalize_view_dag_with_proof(
        loaded.view,
        loaded.view_entity,
        walk.into_outcome(loaded.scope),
        loaded.cgs,
    )
}

/// Walk view nodes via async `runner` (live HTTP; no proof allocation).
pub(crate) async fn run_view_dag_async<R: ViewNodeRunnerAsync + ?Sized>(
    runner: &mut R,
    view_name: &str,
    scope: IndexMap<String, plasm_core::Value>,
    cgs: &CGS,
    ambient: &ViewAmbientContext,
) -> Result<ExecutionResult, RuntimeError> {
    let loaded = LoadedViewDag::load(view_name, scope, cgs, ambient)?;
    let mut walk = ViewDagWalkState::new();
    walk_view_nodes_async(runner, &loaded, &mut walk).await?;

    let outcome = walk.into_outcome(loaded.scope);
    finalize_view_dag_execution(loaded.view, loaded.view_entity, outcome, loaded.cgs)
}
