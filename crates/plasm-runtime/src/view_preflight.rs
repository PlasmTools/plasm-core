//! View DAG preflight — schema stubs + inner CML compile gates (no HTTP).

use std::collections::BTreeMap;

use indexmap::IndexMap;
use plasm_core::{GetExpr, Predicate, QueryExpr, Value, CGS};

use crate::execution::preflight_compile_expr;
use crate::execution::ExecutionResult;
use crate::materialization::SessionMaterialization;
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
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<(), RuntimeError> {
    preflight_view_scoped_with_proof(
        view_name,
        derive_view_query_scope(view_name, query, cgs)?,
        cgs,
        compiled,
        ambient,
        mat,
    )
    .map(|_| ())
}

/// Preflight one view-backed get (derive scope from reference key).
pub fn preflight_view_get(
    view_name: &str,
    get: &GetExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<(), RuntimeError> {
    preflight_view_scoped_with_proof(
        view_name,
        derive_view_get_scope(view_name, get, cgs)?,
        cgs,
        compiled,
        ambient,
        mat,
    )
    .map(|_| ())
}

/// Preflight a scoped view DAG and return structured proof (tests / debug hooks).
pub fn preflight_view_scoped_with_proof(
    view_name: &str,
    scope: IndexMap<String, Value>,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<ViewRunProof, RuntimeError> {
    let runner = PreflightViewNodeRunner {
        cgs,
        compiled,
        ambient,
        mat,
    };
    run_view_dag_sync(&runner, view_name, scope, cgs, ambient).map(|(proof, _)| proof)
}

pub(crate) struct PreflightViewNodeRunner<'a> {
    pub(crate) cgs: &'a CGS,
    pub(crate) compiled: &'a plasm_compile::CompiledCatalog,
    pub(crate) ambient: &'a ViewAmbientContext,
    pub(crate) mat: &'a SessionMaterialization,
}

impl ViewNodeRunner for PreflightViewNodeRunner<'_> {
    fn run_traversal_node(
        &self,
        ctx: &ViewRunContext<'_>,
        node: &plasm_core::schema::ViewNodeSpec,
        source: &ExecutionResult,
    ) -> Result<ExecutionResult, RuntimeError> {
        use plasm_core::{CapabilityKind, RelationMaterialization};
        let view = self.cgs.views.get(ctx.view_name).expect("loaded view");
        let traverse = node.traverse.as_ref().expect("traversal");
        let source_name = self
            .cgs
            .view_node_entity(view, &traverse.node)
            .map_err(|message| RuntimeError::ConfigurationError { message })?;
        let target = self
            .cgs
            .view_node_entity(view, &node.id)
            .map_err(|message| RuntimeError::ConfigurationError { message })?;
        let relation = &self
            .cgs
            .get_entity(source_name.as_str())
            .expect("source")
            .relations[traverse.relation.as_str()];
        let mut mat = SessionMaterialization::new();
        mat.stamp_provided_session_params(
            SessionMaterialization::provide_catalog_key(self.cgs, None),
            ctx.scope.clone(),
        );
        if matches!(
            relation.materialize,
            Some(RelationMaterialization::FromParentGet { .. })
        ) {
            let parent = GetExpr::new(source_name.as_str(), "1");
            preflight_compile_expr(
                &plasm_core::Expr::Get(parent),
                self.cgs,
                self.compiled,
                self.ambient,
                &mat,
            )?;
        }
        let result = match &relation.materialize {
            Some(RelationMaterialization::QueryScoped { capability, param }) => {
                let cap = self
                    .cgs
                    .get_capability(capability.as_str())
                    .expect("validated relation capability");
                let mut values = ctx.scope.clone();
                values.insert(param.to_string(), Value::String("1".into()));
                let predicate = Predicate::and(
                    values
                        .iter()
                        .map(|(k, v)| Predicate::eq(k.as_str(), v.clone()))
                        .collect(),
                );
                let mut query = QueryExpr::filtered(target.as_str(), predicate);
                query.capability_name = Some(cap.name.clone());
                preflight_compile_expr(
                    &plasm_core::Expr::Query(query),
                    self.cgs,
                    self.compiled,
                    self.ambient,
                    &mat,
                )?;
                stub_query_result(cap, self.cgs, &values)?
            }
            Some(RelationMaterialization::QueryScopedBindings {
                capability,
                bindings,
            }) => {
                let cap = self
                    .cgs
                    .get_capability(capability.as_str())
                    .expect("validated relation capability");
                let mut values = ctx.scope.clone();
                for param in bindings.keys() {
                    values.insert(param.to_string(), Value::String("1".into()));
                }
                let predicate = Predicate::and(
                    values
                        .iter()
                        .map(|(k, v)| Predicate::eq(k.as_str(), v.clone()))
                        .collect(),
                );
                let mut query = QueryExpr::filtered(target.as_str(), predicate);
                query.capability_name = Some(cap.name.clone());
                preflight_compile_expr(
                    &plasm_core::Expr::Query(query),
                    self.cgs,
                    self.compiled,
                    self.ambient,
                    &mat,
                )?;
                stub_query_result(cap, self.cgs, &values)?
            }
            _ => {
                let cap = self
                    .cgs
                    .find_capability(&target, CapabilityKind::Get)
                    .ok_or_else(|| RuntimeError::CapabilityNotFound {
                        capability: "get".into(),
                        entity: target.to_string(),
                    })?;
                let mut bound = BTreeMap::new();
                for (key, value) in ctx.scope {
                    bound.insert(
                        key.clone(),
                        crate::view_plan::scalar_string_from_value(value)?,
                    );
                }
                let target_def = self.cgs.get_entity(target.as_str()).expect("target");
                bound.insert(target_def.id_field.to_string(), "1".into());
                bound.insert("id".into(), "1".into());
                let result = stub_get_result(cap, self.cgs, &bound)?;
                let get = GetExpr::from_ref(result.entities[0].reference.clone());
                preflight_compile_expr(
                    &plasm_core::Expr::Get(get),
                    self.cgs,
                    self.compiled,
                    self.ambient,
                    &mat,
                )?;
                result
            }
        };
        let mut result = result;
        result.coverage = source.coverage;
        Ok(result)
    }

    fn run_query_node(
        &self,
        ctx: &ViewRunContext<'_>,
        node: &plasm_core::schema::ViewNodeSpec,
        cap: &plasm_core::CapabilitySchema,
        pred: &Predicate,
        node_fields: &ViewNodeFieldMap,
    ) -> Result<ExecutionResult, RuntimeError> {
        let q = QueryExpr::filtered(cap.domain.as_str(), pred.clone());
        preflight_compile_expr(
            &plasm_core::Expr::Query(q),
            self.cgs,
            self.compiled,
            self.ambient,
            self.mat,
        )
        .map_err(|e| RuntimeError::ConfigurationError {
            message: format!(
                "view `{}` node `{}` (capability `{}`): {e}",
                ctx.view_name, node.id, node.capability
            ),
        })?;
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
        ctx: &ViewRunContext<'_>,
        node: &plasm_core::schema::ViewNodeSpec,
        cap: &plasm_core::CapabilitySchema,
        get: &GetExpr,
        bound: &BTreeMap<String, String>,
    ) -> Result<ExecutionResult, RuntimeError> {
        preflight_compile_expr(
            &plasm_core::Expr::Get(get.clone()),
            self.cgs,
            self.compiled,
            self.ambient,
            self.mat,
        )
        .map_err(|e| RuntimeError::ConfigurationError {
            message: format!(
                "view `{}` node `{}` (capability `{}`): {e}",
                ctx.view_name, node.id, node.capability
            ),
        })?;
        stub_get_result(cap, self.cgs, bound)
    }

    fn run_create_node(
        &self,
        ctx: &ViewRunContext<'_>,
        node: &plasm_core::schema::ViewNodeSpec,
        cap: &plasm_core::CapabilitySchema,
        create: &plasm_core::CreateExpr,
    ) -> Result<ExecutionResult, RuntimeError> {
        preflight_compile_expr(
            &plasm_core::Expr::Create(create.clone()),
            self.cgs,
            self.compiled,
            self.ambient,
            self.mat,
        )
        .map_err(|e| RuntimeError::ConfigurationError {
            message: format!(
                "view `{}` node `{}` (capability `{}`): {e}",
                ctx.view_name, node.id, node.capability
            ),
        })?;
        stub_query_result(cap, self.cgs, &IndexMap::new())
    }
}
