//! CML compile gates without HTTP I/O — shared by dry preflight and live execute.

use super::*;
use crate::preflight::apply_preflight_compile_stubs;
use crate::view_plan::ViewAmbientContext;
use crate::view_plan::{derive_view_get_scope, derive_view_query_scope};
use crate::view_preflight::preflight_view_scoped_with_proof;

struct PreparedViewPreflight {
    name: String,
    scope: indexmap::IndexMap<String, Value>,
    ambient: ViewAmbientContext,
}

/// Compile a relation over a row already admitted by the plan's source binding.
/// The root reference denotes observed input, not an authored request to reread it.
pub fn preflight_compile_bound_relation(
    expr: &Expr,
    observed: &ExecutionResult,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<(), RuntimeError> {
    let Expr::Chain(chain) = expr else {
        return Err(plasm_core::collection_codec::CollectionFault::Conservation.into());
    };
    let Expr::Get(source) = chain.source.as_ref() else {
        return Err(plasm_core::collection_codec::CollectionFault::Conservation.into());
    };
    let parent = cgs
        .get_entity(source.reference.entity_type.as_str())
        .ok_or_else(|| RuntimeError::EntityUnknown {
            entity: source.reference.entity_type.to_string(),
        })?;
    observed
        .collection
        .materialize(plasm_core::collection_codec::Demand::Observed)?;
    let row = observed
        .entities()
        .first()
        .filter(|row| observed.count() == 1 && row.reference == source.reference)
        .ok_or_else(|| {
            RuntimeError::from(plasm_core::collection_codec::CollectionFault::Conservation)
        })?;
    let Some(relation) = parent.relations.get(chain.selector.as_str()) else {
        let field = parent.fields.get(chain.selector.as_str()).ok_or_else(|| {
            crate::ChainError::SelectorMissing {
                entity: parent.name.to_string(),
                selector: chain.selector.to_string(),
            }
        })?;
        let value = cgs.named_value_for_slot(field)?;
        let FieldType::EntityRef { target, .. } = &value.field_type else {
            return Err(crate::ChainError::FieldNotEntityRef {
                entity: parent.name.to_string(),
                field: chain.selector.to_string(),
                actual: value.field_type.clone(),
            }
            .into());
        };
        let reference = super::predicates::extract_ref_id(row, chain.selector.as_str(), cgs)
            .map(|id| Ref::new(target.clone(), id));
        return preflight_bound_targets(
            chain,
            row,
            reference.iter(),
            target,
            cgs,
            compiled,
            ambient,
            mat,
        );
    };
    // Validate the same static read-path contract issued by compiler admission.
    plasm_core::relation_materialize::dependencies::resolve(
        cgs,
        cgs,
        parent.name.as_str(),
        relation.target_resource.as_str(),
        relation.materialize.as_ref(),
    )?;
    let materialize = relation
        .materialize
        .as_ref()
        .unwrap_or(&RelationMaterialization::Unavailable);
    match materialize {
        RelationMaterialization::GetScopedBindings {
            capability,
            bindings,
        } => {
            let cap = cgs.get_capability(capability.as_str()).ok_or_else(|| {
                RuntimeError::CapabilityUnknown {
                    capability: capability.to_string(),
                }
            })?;
            if cap.domain != relation.target_resource {
                return Err(crate::ChainError::CapabilityDomainMismatch {
                    capability: capability.to_string(),
                    expected: relation.target_resource.to_string(),
                    actual: cap.domain.to_string(),
                }
                .into());
            }
            let target = cgs
                .get_entity(relation.target_resource.as_str())
                .ok_or_else(|| RuntimeError::EntityUnknown {
                    entity: relation.target_resource.to_string(),
                })?;
            let bound = bindings
                .iter()
                .map(|(param, field)| {
                    super::scoped_fanout::chain_binding_value(row, parent, field, cgs)
                        .map(|value| (param.to_string(), value))
                })
                .collect::<Result<IndexMap<String, String>, RuntimeError>>()?;
            let reference =
                super::scoped_fanout::ref_from_materialize_bindings_for_get_chain(target, &bound)?;
            let mut get = GetExpr::from_ref(reference);
            get.capability_name = Some(capability.clone());
            let inherited = CapabilityParamEnv::from_source_row_for_cap(cgs, mat, row, cap);
            let target_ambient = ambient
                .clone()
                .with_capability_params(inherited.bindings().clone());
            return preflight_relation_get(&get, cgs, compiled, &target_ambient, mat, &inherited);
        }
        RelationMaterialization::FromParentGet { .. }
        | RelationMaterialization::ViewEmbed { .. } => {
            let refs = row.relations.get(relation.name.as_str()).ok_or_else(|| {
                crate::CacheError::RelationUnobserved {
                    reference: row.reference.clone(),
                    relation: relation.name.to_string(),
                }
            })?;
            return preflight_bound_targets(
                chain,
                row,
                refs.iter(),
                &relation.target_resource,
                cgs,
                compiled,
                ambient,
                mat,
            );
        }
        RelationMaterialization::PreferFromParentGet { fallback, .. } => {
            let (_, jobs) = super::scoped_fanout::partition_prefer_from_parent_get(
                observed.entities(),
                materialize,
                relation.name.as_str(),
                relation.target_resource.as_str(),
                mat,
                parent,
                cgs,
                &relation.target_resource,
                fallback,
            )?;
            for (_, query) in jobs {
                preflight_compile_expr(&Expr::Query(query), cgs, compiled, ambient, mat)?;
            }
            return Ok(());
        }
        RelationMaterialization::Unavailable => {
            if relation.cardinality == plasm_core::Cardinality::Many {
                return Err(crate::ChainError::MaterializationMissing {
                    entity: parent.name.to_string(),
                    relation: relation.name.to_string(),
                }
                .into());
            }
            let reference = super::predicates::extract_ref_id(row, chain.selector.as_str(), cgs)
                .map(|id| Ref::new(relation.target_resource.clone(), id));
            return preflight_bound_targets(
                chain,
                row,
                reference.iter(),
                &relation.target_resource,
                cgs,
                compiled,
                ambient,
                mat,
            );
        }
        RelationMaterialization::QueryScoped { .. }
        | RelationMaterialization::QueryScopedBindings { .. } => {}
    }
    let (name, bindings) = match materialize {
        RelationMaterialization::QueryScopedBindings {
            capability,
            bindings,
            ..
        } => (capability, bindings.clone()),
        RelationMaterialization::QueryScoped {
            capability, param, ..
        } => (
            capability,
            indexmap::IndexMap::from([(param.clone(), parent.id_field.clone())]),
        ),
        _ => unreachable!("non-query materializers return above"),
    };
    let capability =
        cgs.get_capability(name.as_str())
            .ok_or_else(|| RuntimeError::CapabilityUnknown {
                capability: name.to_string(),
            })?;
    let params: Vec<_> = capability.query_surface_fields().cloned().collect();
    let predicates = bindings
        .iter()
        .map(|(param, field)| {
            let raw = super::scoped_fanout::chain_binding_raw_json(row, parent, field, cgs)?;
            let value = super::scoped_fanout::chain_binding_plasm_value(
                &raw,
                param.as_str(),
                &params,
                cgs,
            )?;
            Ok(plasm_core::Predicate::eq(param.as_str(), value))
        })
        .collect::<Result<Vec<_>, RuntimeError>>()?;
    let mut query = QueryExpr::filtered(
        capability.domain.clone(),
        plasm_core::Predicate::and(predicates),
    );
    query.capability_name = Some(name.clone());
    super::hydrate::relation_inherit_for_scoped_query(cgs, mat, row, capability, &query)?
        .apply_to_scoped_query(&mut query);
    preflight_compile_expr(&Expr::Query(query), cgs, compiled, ambient, mat)
}

/// Validate only target work required by observed references, never reread the bound parent.
#[allow(clippy::too_many_arguments)]
fn preflight_bound_targets<'a>(
    chain: &plasm_core::ChainExpr,
    parent: &CachedEntity,
    references: impl Iterator<Item = &'a Ref>,
    target: &EntityName,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<(), RuntimeError> {
    for reference in references {
        if &reference.entity_type != target {
            return Err(RuntimeError::TraversalParentTypeMismatch {
                expected: target.to_string(),
                actual: reference.entity_type.to_string(),
            });
        }
        match &chain.step {
            ChainStep::Explicit { expr } => {
                preflight_compile_expr(expr, cgs, compiled, ambient, mat)?
            }
            ChainStep::AutoGet => {
                let inherited =
                    CapabilityParamEnv::from_source_row(cgs, target.as_str(), mat, parent);
                let target_ambient = ambient
                    .clone()
                    .with_capability_params(inherited.bindings().clone());
                preflight_relation_get(
                    &GetExpr::from_ref(reference.clone()),
                    cgs,
                    compiled,
                    &target_ambient,
                    mat,
                    &inherited,
                )?;
            }
        }
    }
    Ok(())
}

fn preflight_relation_get(
    get: &GetExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
    inherited: &CapabilityParamEnv,
) -> Result<(), RuntimeError> {
    let request = match super::chain::prepare_relation_get(get, inherited, cgs, mat, ambient)? {
        super::chain::PreparedRelationGet::Observed(_) => return Ok(()),
        super::chain::PreparedRelationGet::Fetch(request) => request,
    };
    if let Some(view) = preflight_compile_resolved_get(&request, cgs, compiled)? {
        preflight_view_scoped_with_proof(
            &view.name,
            view.scope,
            cgs,
            compiled,
            &view.ambient,
            mat,
        )?;
    }
    Ok(())
}

/// Compile capability templates for `expr` without dispatching HTTP.
///
/// Identity GETs inherit non-identity capability params from `mat`.
/// Canonical target identity is sealed after input binding. Dry-run and live share this gate.
pub fn preflight_compile_expr(
    expr: &Expr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<(), RuntimeError> {
    if let Some(view) = preflight_compile_leaf(expr, cgs, compiled, ambient, mat)? {
        preflight_view_scoped_with_proof(
            &view.name,
            view.scope,
            cgs,
            compiled,
            &view.ambient,
            mat,
        )?;
    }
    Ok(())
}

/// A view node compiles one transport operation; it cannot invoke the DAG driver.
/// Nested compositions are catalog faults, as in static view validation.
pub(crate) fn preflight_compile_view_node(
    expr: &Expr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
    view: &str,
    node: &str,
) -> Result<(), RuntimeError> {
    if preflight_compile_leaf(expr, cgs, compiled, ambient, mat)?.is_some() {
        return Err(plasm_compile::CatalogTemplateError::NestedView {
            view: view.to_owned(),
            node: node.to_owned(),
        }
        .into());
    }
    Ok(())
}

fn preflight_compile_leaf(
    mut expr: &Expr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    mat: &SessionMaterialization,
) -> Result<Option<PreparedViewPreflight>, RuntimeError> {
    loop {
        match expr {
            Expr::Chain(chain) => expr = &chain.source,
            Expr::Query(query) => return preflight_compile_query(query, cgs, compiled, ambient),
            Expr::Get(get) => {
                return preflight_compile_get(
                    get,
                    cgs,
                    compiled,
                    ambient,
                    mat,
                    GetPurpose::Authored,
                )
            }
            Expr::Create(create) => {
                return preflight_compile_create(create, cgs, compiled).map(|_| None)
            }
            Expr::Delete(delete) => {
                return preflight_compile_delete(delete, cgs, compiled, mat).map(|_| None)
            }
            Expr::Invoke(invoke) => {
                return preflight_compile_invoke(invoke, cgs, compiled, mat).map(|_| None)
            }
            Expr::Page(_) | Expr::Wait(_) | Expr::Cancel(_) | Expr::TeachingValue { .. } => {
                return Ok(None)
            }
        }
    }
}

fn preflight_compile_query(
    query: &QueryExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
) -> Result<Option<PreparedViewPreflight>, RuntimeError> {
    plasm_core::type_check_query(query, cgs)?;
    let prepared = super::query_prepare::prepare(query, cgs)?;
    let capability = prepared.capability;
    let env = prepared.env;
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
    if let CapabilityTemplate::View(vt) = &capability_template {
        return Ok(Some(PreparedViewPreflight {
            name: vt.view.to_string(),
            scope: derive_view_query_scope(vt.view.as_str(), query, cgs, &prepared.bindings)?,
            ambient: ambient.clone(),
        }));
    }
    compile_operation_dispatch(&capability_template, &env).map(|_| None)
}

fn preflight_compile_get(
    get: &GetExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    ambient: &ViewAmbientContext,
    bindings: &impl GetBindings,
    purpose: GetPurpose,
) -> Result<Option<PreparedViewPreflight>, RuntimeError> {
    let request = ResolvedGet::resolve(
        get,
        cgs,
        get.capability_name.as_deref(),
        bindings,
        ambient,
        purpose,
    )?;
    preflight_compile_resolved_get(&request, cgs, compiled)
}

fn preflight_compile_resolved_get(
    request: &ResolvedGet<'_>,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
) -> Result<Option<PreparedViewPreflight>, RuntimeError> {
    let get = request.get;
    let capability = request.capability;
    // List-backed derived Gets have no CML mapping; schema load already validated the plan.
    if capability.derived.is_some() {
        return Ok(None);
    }
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
    if let CapabilityTemplate::View(vt) = &capability_template {
        return Ok(Some(PreparedViewPreflight {
            name: vt.view.to_string(),
            scope: derive_view_get_scope(vt.view.as_str(), get, cgs)?,
            ambient: request.ambient.clone(),
        }));
    }
    compile_operation_dispatch(&capability_template, &request.env).map(|_| None)
}

fn preflight_compile_create(
    create: &plasm_core::CreateExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
) -> Result<(), RuntimeError> {
    let (capability, mut env) = super::mutation_prepare::create(create, cgs)?;
    let template = compiled.capability(capability.name.as_str())?;
    apply_preflight_compile_stubs(&mut env, capability, cgs);
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(template, &env).map(|_| ())
}

fn preflight_compile_delete(
    delete: &plasm_core::DeleteExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    mat: &SessionMaterialization,
) -> Result<(), RuntimeError> {
    let (capability, _, mut env) = super::mutation_prepare::targeted(delete, cgs, mat)?;
    let template = compiled.capability(capability.name.as_str())?;
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(template, &env).map(|_| ())
}

fn preflight_compile_invoke(
    invoke: &InvokeExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
    mat: &SessionMaterialization,
) -> Result<(), RuntimeError> {
    let (capability, _, mut env) = super::mutation_prepare::invoke(invoke, cgs, mat)?;
    let template = compiled.capability(capability.name.as_str())?;
    apply_preflight_compile_stubs(&mut env, capability, cgs);
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(template, &env).map(|_| ())
}
