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
            return preflight_compile_expr(&Expr::Get(get), cgs, compiled, &target_ambient, mat);
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
            indexmap::IndexMap::from([(param.clone(), parent.id_field.clone().into())]),
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
                if !super::chain::observed_row_needs_no_detail_get(mat.get(reference), cgs) {
                    let inherited =
                        CapabilityParamEnv::from_source_row(cgs, target.as_str(), mat, parent);
                    let target_ambient = ambient
                        .clone()
                        .with_capability_params(inherited.bindings().clone());
                    preflight_compile_expr(
                        &Expr::Get(GetExpr::from_ref(reference.clone())),
                        cgs,
                        compiled,
                        &target_ambient,
                        mat,
                    )?;
                }
            }
        }
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
            Expr::Get(get) => return preflight_compile_get(get, cgs, compiled, ambient, mat),
            Expr::Create(create) => {
                return preflight_compile_create(create, cgs, compiled).map(|_| None)
            }
            Expr::Delete(delete) => {
                return preflight_compile_delete(delete, cgs, compiled).map(|_| None)
            }
            Expr::Invoke(invoke) => {
                return preflight_compile_invoke(invoke, cgs, compiled).map(|_| None)
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
    let filter = compile_query_dispatch(query, cgs)?;
    let capability = resolve_query_capability(query, cgs)?;
    let mut env = CmlEnv::new();
    if let Some(f) = &filter {
        let json_val = f.to_json();
        env.insert("filter".to_string(), json_to_plasm_value(&json_val));
    }
    if let Some(pred) = &query.predicate {
        extract_predicate_vars(pred, &mut env);
    }
    normalize_cml_env_inputs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
    if let Some(proj) = &query.projection {
        env.insert(
            "projection".to_string(),
            Value::Array(proj.iter().map(|s| Value::String(s.clone())).collect()),
        );
    }
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
    if let CapabilityTemplate::View(vt) = &capability_template {
        return Ok(Some(PreparedViewPreflight {
            name: vt.view.to_string(),
            scope: derive_view_query_scope(vt.view.as_str(), query, cgs)?,
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
    mat: &SessionMaterialization,
) -> Result<Option<PreparedViewPreflight>, RuntimeError> {
    let request = ResolvedGet::resolve(
        get,
        cgs,
        get.capability_name.as_deref(),
        mat,
        ambient,
        GetPurpose::Authored,
    )?;
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
            ambient: request.ambient,
        }));
    }
    compile_operation_dispatch(&capability_template, &request.env).map(|_| None)
}

fn preflight_compile_create(
    create: &plasm_core::CreateExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
) -> Result<(), RuntimeError> {
    let capability = cgs
        .get_capability(create.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: create.capability.to_string(),
            entity: create.entity.to_string(),
        })?;
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
    let payload = if let Some(schema) = &capability.inputs.payload {
        InvokeInputPayload::lift(&create.input.to_value(), &schema.input_type, cgs)
    } else {
        create.input.clone()
    };
    let input = match capability.inputs.payload.as_ref() {
        Some(schema) => plasm_core::normalize_structured_string_inputs(
            payload.to_value(),
            &schema.input_type,
            cgs,
        ),
        None => payload.to_value(),
    };
    let input = plasm_core::prepare_create_capability_input(capability, create, input, cgs);
    let mut env = CmlEnv::new();
    env.insert("input".to_string(), input.clone());
    if let Value::Object(ref map) = input {
        for (k, v) in map {
            env.insert(k.clone(), v.clone());
        }
    }
    normalize_cml_env_inputs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
    apply_preflight_compile_stubs(&mut env, capability, cgs);
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

fn preflight_compile_delete(
    delete: &plasm_core::DeleteExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
) -> Result<(), RuntimeError> {
    let capability = cgs
        .get_capability(delete.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: delete.capability.to_string(),
            entity: delete.target.entity_type.to_string(),
        })?;
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
    let input_for_env = targeted_call_input(delete, capability, cgs);
    let mut env = CmlEnv::new();
    let target_ent = cgs
        .get_entity(delete.target.entity_type.as_str())
        .ok_or_else(|| RuntimeError::EntityUnknown {
            entity: delete.target.entity_type.to_string(),
        })?;
    populate_template_path_env(
        &mut env,
        capability,
        &delete.target,
        plasm_core::IdentityProjectionCtx::Entity(target_ent),
        input_for_env.as_ref(),
    )?;
    if let Some(input) = input_for_env {
        env.insert("input".to_string(), input);
    }
    normalize_cml_env_inputs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

fn preflight_compile_invoke(
    invoke: &InvokeExpr,
    cgs: &CGS,
    compiled: &plasm_compile::CompiledCatalog,
) -> Result<(), RuntimeError> {
    let capability = cgs
        .get_capability(invoke.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: invoke.capability.to_string(),
            entity: invoke.target.entity_type.to_string(),
        })?;
    let capability_template = compiled.capability(capability.name.as_str())?.clone();
    let input_for_env = targeted_call_input(invoke, capability, cgs);
    let mut env = CmlEnv::new();
    let target_ent = cgs
        .get_entity(invoke.target.entity_type.as_str())
        .ok_or_else(|| RuntimeError::EntityUnknown {
            entity: invoke.target.entity_type.to_string(),
        })?;
    populate_template_path_env(
        &mut env,
        capability,
        &invoke.target,
        plasm_core::IdentityProjectionCtx::Entity(target_ent),
        input_for_env.as_ref(),
    )?;
    if let Some(input) = &input_for_env {
        env.insert("input".to_string(), input.clone());
    }
    normalize_cml_env_inputs(&mut env, cgs, capability)?;
    plasm_core::apply_entity_ref_scope_splat(&mut env, cgs, capability)?;
    merge_entity_id_from_into_input_env(&mut env, Some(target_ent), capability);
    apply_preflight_compile_stubs(&mut env, capability, cgs);
    merge_plasm_execute_session_env(&mut env);
    compile_operation_dispatch(&capability_template, &env).map(|_| ())
}

pub(super) fn targeted_call_input(
    invoke: &impl plasm_core::expr::TargetedCall,
    capability: &plasm_core::CapabilitySchema,
    cgs: &CGS,
) -> Option<Value> {
    let raw = match invoke.input() {
        None => Value::Object(indexmap::IndexMap::new()),
        Some(input) => {
            let payload = if let Some(schema) = &capability.inputs.payload {
                InvokeInputPayload::lift(&input.to_value(), &schema.input_type, cgs)
            } else {
                input.clone()
            };
            match capability.inputs.payload.as_ref() {
                Some(schema) => plasm_core::normalize_structured_string_inputs(
                    payload.to_value(),
                    &schema.input_type,
                    cgs,
                ),
                None => payload.to_value(),
            }
        }
    };
    let effective = plasm_core::prepare_targeted_capability_input(capability, invoke, raw, cgs);
    if invoke.input().is_none() && effective.as_object().is_some_and(|m| m.is_empty()) {
        None
    } else {
        Some(effective)
    }
}
