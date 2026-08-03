//! Capability **preflight** orchestration (ordered steps before CML compile).

use crate::execution::{ExecutionEngine, ExecutionMode, StreamConsumeOpts};
use crate::materialization::SessionMaterialization;
use crate::view_plan::ViewAmbientContext;
use crate::{CachedEntity, EntityCompleteness, RuntimeError};
use indexmap::IndexMap;
use plasm_compile::CmlEnv;
use plasm_core::preflight::{
    ExistenceOnExists, PickSpec, PreflightFieldPath, PreflightPlan, PreflightStep, ScopeBind,
};
use plasm_core::TypedFieldValue;
use plasm_core::{
    CapabilityKind, CapabilitySchema, EntityDef, GetExpr, InvokeExpr, Predicate, QueryExpr, Ref,
    Value, CGS,
};
use std::collections::HashSet;

use crate::compile_stub_value::{preflight_compile_stub_value, STUB_STRING, ZERO_UUID};

pub(crate) fn merge_preflight_fields_into_env(
    env: &mut CmlEnv,
    prefix: &str,
    fields: &IndexMap<String, TypedFieldValue>,
) {
    for (field_name, value) in fields {
        env.insert(format!("{prefix}_{field_name}"), value.to_value());
    }
}

pub(crate) struct PreflightInvoke<'a> {
    pub invoke: &'a InvokeExpr,
}

/// Run declarative preflight steps after invoke/create env assembly.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_preflight_steps(
    engine: &ExecutionEngine,
    capability: &CapabilitySchema,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    env: &mut CmlEnv,
    invoke: Option<PreflightInvoke<'_>>,
    is_create: bool,
) -> Result<(), RuntimeError> {
    let Some(PreflightPlan(steps)) = capability.preflight.as_ref() else {
        return Ok(());
    };
    for step in steps {
        match step {
            PreflightStep::HydrateInvokeTarget { get, prefix } => {
                if is_create {
                    continue;
                }
                let Some(PreflightInvoke { invoke }) = invoke else {
                    continue;
                };
                hydrate_invoke_target(
                    engine, capability, cgs, cache, mode, env, invoke, get, prefix,
                )
                .await?;
            }
            PreflightStep::HydrateEntityRefParam { param, get, merge } => {
                if !env_param_present(env, param) {
                    continue;
                }
                hydrate_entity_ref_param(
                    engine, cgs, cache, mode, env, capability, param, get, merge,
                )
                .await?;
            }
            PreflightStep::QueryPick {
                when,
                query,
                scope,
                pick,
                merge,
            } => {
                if let Some(w) = when {
                    if !env_param_present(env, w) {
                        continue;
                    }
                }
                query_pick_step(
                    engine, cgs, cache, mode, env, capability, query, scope, pick, merge,
                )
                .await?;
            }
            PreflightStep::LabelIdsDelta {
                add_when,
                remove_when,
                lookup,
                from_preflight,
                merge,
            } => {
                let add = env_param_present(env, add_when);
                let remove = env_param_present(env, remove_when);
                if !add && !remove {
                    continue;
                }
                label_ids_delta_step(
                    engine,
                    cgs,
                    cache,
                    mode,
                    env,
                    add_when,
                    remove_when,
                    lookup,
                    from_preflight,
                    merge,
                    add,
                    remove,
                )
                .await?;
            }
            PreflightStep::ExistenceCheck {
                query, on_exists, ..
            } => {
                existence_check_step(engine, cgs, cache, mode, env, capability, query, *on_exists)
                    .await?;
            }
        }
    }
    Ok(())
}

/// Compile-only preflight: inject stub merge keys so CML templates compile without HTTP hydration.
///
/// Invoke-target stubs cover the referenced Get entity's declared fields, preserving the catalog
/// contract that `provides` does not strip decoded fields. This compile environment is necessarily
/// a schema-derived superset: live hydration merges only fields actually present in the decoded
/// row. Undeclared CML variables such as `ds_typo` are still rejected.
pub(crate) fn apply_preflight_compile_stubs(
    env: &mut CmlEnv,
    capability: &CapabilitySchema,
    cgs: &CGS,
) -> Result<(), RuntimeError> {
    let Some(PreflightPlan(steps)) = capability.preflight.as_ref() else {
        return Ok(());
    };
    for step in steps {
        match step {
            PreflightStep::HydrateEntityRefParam { param, merge, .. } => {
                if !env_param_present(env, param) {
                    continue;
                }
                for wire_key in merge.keys() {
                    if env.get(wire_key).is_none() {
                        env.insert(
                            wire_key.clone(),
                            preflight_wire_key_compile_stub_value(wire_key),
                        );
                    }
                }
            }
            PreflightStep::QueryPick { when, merge, .. } => {
                if let Some(w) = when {
                    if !env_param_present(env, w) {
                        continue;
                    }
                }
                for wire_key in merge.keys() {
                    if env.get(wire_key).is_none() {
                        env.insert(
                            wire_key.clone(),
                            preflight_wire_key_compile_stub_value(wire_key),
                        );
                    }
                }
            }
            PreflightStep::LabelIdsDelta {
                merge,
                add_when,
                remove_when,
                ..
            } => {
                if (env_param_present(env, add_when) || env_param_present(env, remove_when))
                    && env.get(merge.as_str()).is_none()
                {
                    env.insert(merge.clone(), Value::Array(Vec::new()));
                }
            }
            PreflightStep::HydrateInvokeTarget { get, prefix } => {
                // Schema validation rejects this step on Create. Match live preflight's defensive
                // behavior if an unvalidated CGS is passed directly to the runtime.
                if capability.kind != CapabilityKind::Create {
                    add_hydrate_invoke_target_compile_stubs(env, capability, cgs, get, prefix)?;
                }
            }
            PreflightStep::ExistenceCheck { .. } => {}
        }
    }
    Ok(())
}

struct HydrateInvokeTargetContract<'a> {
    prefix: String,
    get: &'a CapabilitySchema,
    entity: &'a EntityDef,
}

/// Resolve the schema contract shared by compile-only stubbing and live hydration.
///
/// `CGS::validate` owns these invariants for loaded catalogs. The checks here are a defensive
/// boundary for callers that construct a CGS programmatically and invoke runtime APIs directly.
fn resolve_hydrate_invoke_target_contract<'a>(
    capability: &CapabilitySchema,
    cgs: &'a CGS,
    get_name: &str,
    prefix: &str,
) -> Result<HydrateInvokeTargetContract<'a>, RuntimeError> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Err(RuntimeError::ConfigurationError {
            message: format!(
                "preflight hydrate_invoke_target on capability '{}': prefix must not be empty",
                capability.name
            ),
        });
    }
    let get = cgs.get_capability(get_name).ok_or_else(|| {
        RuntimeError::ConfigurationError {
            message: format!(
                "preflight hydrate_invoke_target on capability '{}': Get capability '{}' was not found",
                capability.name, get_name
            ),
        }
    })?;
    if get.kind != CapabilityKind::Get {
        return Err(RuntimeError::ConfigurationError {
            message: format!(
                "preflight hydrate_invoke_target on capability '{}': capability '{}' must be kind get",
                capability.name, get_name
            ),
        });
    }
    if get.domain != capability.domain {
        return Err(RuntimeError::ConfigurationError {
            message: format!(
                "preflight hydrate_invoke_target on capability '{}': Get '{}' is for entity {}, expected {}",
                capability.name, get_name, get.domain, capability.domain
            ),
        });
    }
    let entity = cgs.get_entity(get.domain.as_str()).ok_or_else(|| {
        RuntimeError::ConfigurationError {
            message: format!(
                "preflight hydrate_invoke_target on capability '{}': entity '{}' for Get '{}' was not found",
                capability.name, get.domain, get_name
            ),
        }
    })?;
    Ok(HydrateInvokeTargetContract {
        prefix: prefix.to_string(),
        get,
        entity,
    })
}

fn add_hydrate_invoke_target_compile_stubs(
    env: &mut CmlEnv,
    capability: &CapabilitySchema,
    cgs: &CGS,
    get_name: &str,
    prefix: &str,
) -> Result<(), RuntimeError> {
    let contract = resolve_hydrate_invoke_target_contract(capability, cgs, get_name, prefix)?;
    for (field_name, field) in &contract.entity.fields {
        let env_key = format!("{}_{field_name}", contract.prefix);
        let named_value = cgs.named_value_for_slot(field).map_err(|error| {
            RuntimeError::ConfigurationError {
                message: format!(
                    "preflight hydrate_invoke_target on capability '{}': cannot resolve type of field '{}': {error}",
                    capability.name, field_name
                ),
            }
        })?;
        // Live hydration treats this prefix as authoritative and overwrites colliding input keys.
        // Compile-only hydration must do the same so both paths compile against the same namespace.
        env.insert(env_key, preflight_compile_stub_value(named_value, cgs));
    }
    Ok(())
}

fn preflight_wire_key_compile_stub_value(wire_key: &str) -> Value {
    if wire_key.ends_with("Id")
        || wire_key.ends_with("_id")
        || wire_key == "id"
        || wire_key.ends_with("Ids")
    {
        Value::String(ZERO_UUID.to_string())
    } else {
        Value::String(STUB_STRING.to_string())
    }
}

fn env_param_present(env: &CmlEnv, name: &str) -> bool {
    matches!(env.get(name), Some(v) if !matches!(v, Value::Null))
}

#[allow(clippy::too_many_arguments)]
async fn hydrate_invoke_target(
    engine: &ExecutionEngine,
    capability: &CapabilitySchema,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    env: &mut CmlEnv,
    invoke: &InvokeExpr,
    get_name: &str,
    prefix: &str,
) -> Result<(), RuntimeError> {
    let contract = resolve_hydrate_invoke_target_contract(capability, cgs, get_name, prefix)?;

    if let Some(entity) = cache.get(&invoke.target) {
        if entity.completeness == EntityCompleteness::Complete {
            merge_preflight_fields_into_env(env, &contract.prefix, &entity.fields);
            return Ok(());
        }
    }

    let get = GetExpr {
        reference: invoke.target.clone(),
        path_vars: None,
        catalog_entry_id: plasm_core::CatalogEntryStamp::none(),
        capability_name: None,
    };
    let (cached, _source) = engine
        .fetch_get_decoded(
            &get,
            cgs,
            mode,
            Some(contract.get.name.as_str()),
            false,
            Some(cache),
            &ViewAmbientContext::default(),
        )
        .await?;
    cache.insert(cached.clone())?;
    merge_preflight_fields_into_env(env, &contract.prefix, &cached.fields);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn hydrate_entity_ref_param(
    engine: &ExecutionEngine,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    env: &mut CmlEnv,
    capability: &CapabilitySchema,
    param: &str,
    get_cap: &str,
    merge: &IndexMap<String, String>,
) -> Result<(), RuntimeError> {
    let get_capability =
        cgs.get_capability(get_cap)
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: get_cap.to_string(),
                entity: capability.domain.to_string(),
            })?;
    let ent = cgs
        .get_entity(get_capability.domain.as_str())
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!("preflight: unknown entity {}", get_capability.domain),
        })?;
    let reference = ref_from_param_env(env, ent, param)?;
    let get = GetExpr {
        reference,
        path_vars: None,
        catalog_entry_id: plasm_core::CatalogEntryStamp::none(),
        capability_name: None,
    };
    let (cached, _source) = engine
        .fetch_get_decoded(
            &get,
            cgs,
            mode,
            Some(get_cap),
            false,
            Some(cache),
            &ViewAmbientContext::default(),
        )
        .await?;
    for (wire_key, field) in merge {
        let v =
            cached
                .fields
                .get(field.as_str())
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!(
                        "preflight hydrate_entity_ref_param: get '{}' did not provide field '{}'",
                        get_cap, field
                    ),
                })?;
        env.insert(wire_key.clone(), v.to_value());
    }
    Ok(())
}

fn ref_from_param_env(env: &CmlEnv, ent: &EntityDef, param: &str) -> Result<Ref, RuntimeError> {
    let v = env
        .get(param)
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!("preflight: missing param '{param}' in env"),
        })?;
    let id_str = match v {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Object(map) => {
            let key = ent
                .key_vars
                .first()
                .map(|k| k.as_str())
                .unwrap_or(ent.id_field.as_str());
            map.get(key)
                .and_then(|x| match x {
                    Value::String(s) => Some(s.clone()),
                    Value::Integer(i) => Some(i.to_string()),
                    _ => None,
                })
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("preflight: param '{param}' object missing key '{key}'"),
                })?
        }
        _ => {
            return Err(RuntimeError::ConfigurationError {
                message: format!("preflight: param '{param}' is not a scalar entity ref"),
            });
        }
    };
    Ok(Ref::new(ent.name.clone(), id_str))
}

#[allow(clippy::too_many_arguments)]
async fn query_pick_step(
    engine: &ExecutionEngine,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    env: &mut CmlEnv,
    parent_cap: &CapabilitySchema,
    query_cap_name: &str,
    scope: &IndexMap<String, ScopeBind>,
    pick: &PickSpec,
    merge: &IndexMap<String, String>,
) -> Result<(), RuntimeError> {
    let query_cap =
        cgs.get_capability(query_cap_name)
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: query_cap_name.to_string(),
                entity: parent_cap.domain.to_string(),
            })?;
    let predicate = scope_to_predicate(env, scope)?;
    let mut query = QueryExpr::filtered(query_cap.domain.clone(), predicate);
    query.capability_name = Some(query_cap.name.clone());

    let res = engine
        .execute_query(
            &query,
            cgs,
            cache,
            mode,
            StreamConsumeOpts::default(),
            &ViewAmbientContext::default(),
        )
        .await?;

    let needle = env
        .get(&pick.equals_param)
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!(
                "preflight query_pick: missing equals_param '{}' in env",
                pick.equals_param
            ),
        })?;
    let needle_str = value_to_match_string(needle);

    let mut matches: Vec<&CachedEntity> = Vec::new();
    for entity in &res.entities {
        let Some(tf) = entity.fields.get(pick.field.as_str()) else {
            continue;
        };
        if value_to_match_string(&tf.to_value()) == needle_str {
            matches.push(entity);
        }
    }

    match matches.len() {
        0 => Err(RuntimeError::ConfigurationError {
            message: format!(
                "preflight query_pick: no row where {} == {} in first page of '{}'",
                pick.field, pick.equals_param, query_cap_name
            ),
        }),
        1 => {
            let row = matches[0];
            for (wire_key, field) in merge {
                let v = row.fields.get(field.as_str()).ok_or_else(|| {
                    RuntimeError::ConfigurationError {
                        message: format!(
                            "preflight query_pick: row missing field '{field}' for wire key '{wire_key}'"
                        ),
                    }
                })?;
                env.insert(wire_key.clone(), v.to_value());
            }
            Ok(())
        }
        n => Err(RuntimeError::ConfigurationError {
            message: format!(
                "preflight query_pick: {n} rows match {} == {} in '{}' (ambiguous)",
                pick.field, pick.equals_param, query_cap_name
            ),
        }),
    }
}

fn scope_to_predicate(
    env: &CmlEnv,
    scope: &IndexMap<String, ScopeBind>,
) -> Result<Predicate, RuntimeError> {
    let mut preds = Vec::new();
    for (param, bind) in scope {
        let v = resolve_scope_bind(env, bind)?;
        preds.push(Predicate::eq(param.as_str(), v));
    }
    Ok(match preds.len() {
        0 => Predicate::True,
        1 => preds.into_iter().next().unwrap(),
        _ => Predicate::and(preds),
    })
}

fn resolve_scope_bind(env: &CmlEnv, bind: &ScopeBind) -> Result<Value, RuntimeError> {
    if let Some(p) = &bind.from_param {
        return env
            .get(p)
            .cloned()
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!("preflight scope: missing from_param '{p}'"),
            });
    }
    if let Some(path) = &bind.from_preflight {
        return value_at_preflight_path(env, path);
    }
    if let Some(lit) = &bind.literal {
        return Ok(lit.clone());
    }
    Err(RuntimeError::ConfigurationError {
        message: "preflight scope bind: one of from_param, from_preflight, literal required"
            .to_string(),
    })
}

fn value_at_preflight_path(env: &CmlEnv, path: &PreflightFieldPath) -> Result<Value, RuntimeError> {
    if path.path.is_empty() {
        return Err(RuntimeError::ConfigurationError {
            message: "preflight from_preflight.path must not be empty".to_string(),
        });
    }
    let top_key = format!("{}_{}", path.prefix, path.path[0]);
    let mut cur = env
        .get(&top_key)
        .cloned()
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!("preflight path: missing env key '{top_key}'"),
        })?;
    for seg in path.path.iter().skip(1) {
        cur = match cur {
            Value::Object(mut map) => {
                map.swap_remove(seg)
                    .ok_or_else(|| RuntimeError::ConfigurationError {
                        message: format!("preflight path: missing segment '{seg}'"),
                    })?
            }
            _ => {
                return Err(RuntimeError::ConfigurationError {
                    message: format!("preflight path: cannot descend into '{seg}'"),
                });
            }
        };
    }
    Ok(cur)
}

fn value_to_match_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => format!("{other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn label_ids_delta_step(
    engine: &ExecutionEngine,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    env: &mut CmlEnv,
    add_when: &str,
    remove_when: &str,
    lookup_cap: &str,
    from_preflight: &PreflightFieldPath,
    merge_key: &str,
    do_add: bool,
    do_remove: bool,
) -> Result<(), RuntimeError> {
    let mut ids: HashSet<String> = HashSet::new();
    if let Ok(Value::Object(map)) = value_at_preflight_path(env, from_preflight) {
        if let Some(Value::Array(nodes)) = map.get("nodes") {
            for node in nodes {
                if let Value::Object(row) = node {
                    if let Some(Value::String(id)) = row.get("id") {
                        ids.insert(id.clone());
                    }
                }
            }
        }
    }

    let lookup_cap_schema =
        cgs.get_capability(lookup_cap)
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: lookup_cap.to_string(),
                entity: String::new(),
            })?;

    if do_add {
        let name = env
            .get(add_when)
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!("preflight label_ids_delta: missing '{add_when}'"),
            })?;
        let name_str = value_to_match_string(name);
        let id = resolve_label_id_by_name(engine, cgs, cache, mode, lookup_cap_schema, &name_str)
            .await?;
        ids.insert(id);
    }
    if do_remove {
        let name = env
            .get(remove_when)
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!("preflight label_ids_delta: missing '{remove_when}'"),
            })?;
        let name_str = value_to_match_string(name);
        if let Ok(id) =
            resolve_label_id_by_name(engine, cgs, cache, mode, lookup_cap_schema, &name_str).await
        {
            ids.remove(&id);
        }
    }

    let arr: Vec<Value> = ids.into_iter().map(Value::String).collect();
    env.insert(merge_key.to_string(), Value::Array(arr));
    Ok(())
}

async fn resolve_label_id_by_name(
    engine: &ExecutionEngine,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    lookup_cap: &CapabilitySchema,
    name: &str,
) -> Result<String, RuntimeError> {
    let mut query = QueryExpr::filtered(lookup_cap.domain.clone(), Predicate::True);
    query.capability_name = Some(lookup_cap.name.clone());
    let res = engine
        .execute_query(
            &query,
            cgs,
            cache,
            mode,
            StreamConsumeOpts::default(),
            &ViewAmbientContext::default(),
        )
        .await?;
    let mut matches = Vec::new();
    for row in &res.entities {
        if let Some(tf) = row.fields.get("name") {
            if value_to_match_string(&tf.to_value()) == name {
                if let Some(id_tf) = row.fields.get("id") {
                    matches.push(value_to_match_string(&id_tf.to_value()));
                }
            }
        }
    }
    match matches.len() {
        0 => Err(RuntimeError::ConfigurationError {
            message: format!("preflight label_ids_delta: no label named '{name}'"),
        }),
        1 => Ok(matches.into_iter().next().unwrap()),
        n => Err(RuntimeError::ConfigurationError {
            message: format!("preflight label_ids_delta: {n} labels named '{name}'"),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
async fn existence_check_step(
    engine: &ExecutionEngine,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
    env: &mut CmlEnv,
    capability: &CapabilitySchema,
    query_cap: &str,
    on_exists: ExistenceOnExists,
) -> Result<(), RuntimeError> {
    let qcap = cgs
        .get_capability(query_cap)
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!("existence_check: unknown capability '{query_cap}'"),
        })?;
    let mut pred = Predicate::True;
    if let Some(keys) = &capability.identity_key {
        for key in keys {
            if let Some(v) = env.get(key) {
                pred = Predicate::And {
                    args: vec![
                        pred,
                        Predicate::Comparison {
                            field: key.clone(),
                            op: plasm_core::CompOp::Eq,
                            value: v.clone().into(),
                        },
                    ],
                };
            }
        }
    }
    let q = QueryExpr::filtered(qcap.domain.as_str(), pred);
    let res = engine
        .execute_query(
            &q,
            cgs,
            cache,
            mode,
            StreamConsumeOpts::default(),
            &ViewAmbientContext::default(),
        )
        .await?;
    if res.count > 0 {
        match on_exists {
            ExistenceOnExists::Fail => {
                return Err(RuntimeError::WorkflowConflict {
                    conflict: Box::new(plasm_core::WorkflowConflict {
                        kind: plasm_core::WorkflowConflictKind::ResourceExists,
                        entity: capability.domain.to_string(),
                        key: IndexMap::new(),
                        hint: format!(
                            "preflight existence_check: {} already exists",
                            capability.name
                        ),
                        existing: None,
                    }),
                    message: format!(
                        "preflight existence_check failed for capability '{}'",
                        capability.name
                    ),
                    attempts: 1,
                });
            }
            ExistenceOnExists::SkipWrite => {
                env.insert(
                    plasm_core::preflight::PLASM_EXISTENCE_SKIP_WRITE_ENV.to_string(),
                    Value::Bool(true),
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::Predicate;

    fn hydrate_fixture() -> CGS {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/schemas/hydrate_invoke_target");
        plasm_core::load_schema(&dir).expect("load hydrate_invoke_target fixture")
    }

    #[test]
    fn hydrate_invoke_target_compile_stub_overwrites_existing_env_like_live_hydration() {
        let cgs = hydrate_fixture();
        let capability = cgs
            .get_capability("datasource_run")
            .expect("datasource_run capability");
        let mut env = CmlEnv::new();
        env.insert(
            "ds_type".to_string(),
            Value::String("already-present".to_string()),
        );

        apply_preflight_compile_stubs(&mut env, capability, &cgs).expect("compile stubs");

        assert_eq!(
            env.get("ds_type"),
            Some(&Value::String(STUB_STRING.to_string()))
        );
    }

    #[test]
    fn scope_bind_from_param_builds_predicate() {
        let mut env = CmlEnv::new();
        env.insert("team_key".to_string(), Value::String("ENG".into()));
        let mut scope = IndexMap::new();
        scope.insert(
            "team_key".to_string(),
            ScopeBind {
                from_param: Some("team_key".to_string()),
                ..Default::default()
            },
        );
        let p = scope_to_predicate(&env, &scope).unwrap();
        assert!(matches!(
            p,
            Predicate::Comparison { field, .. } if field == "team_key"
        ));
    }
}
