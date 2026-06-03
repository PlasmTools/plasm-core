//! Capability **preflight** orchestration (ordered steps before CML compile).

use crate::execution::{ExecutionEngine, ExecutionMode, StreamConsumeOpts};
use crate::{CachedEntity, EntityCompleteness, GraphCache, RuntimeError};
use indexmap::IndexMap;
use plasm_compile::CmlEnv;
use plasm_core::preflight::{
    PickSpec, PreflightFieldPath, PreflightPlan, PreflightStep, ScopeBind,
};
use plasm_core::TypedFieldValue;
use plasm_core::{
    CapabilitySchema, EntityDef, GetExpr, InvokeExpr, Predicate, QueryExpr, Ref, Value, CGS,
};
use std::collections::HashSet;

pub(crate) fn merge_preflight_fields_into_env(
    env: &mut CmlEnv,
    prefix: &str,
    fields: &IndexMap<String, TypedFieldValue>,
) {
    for (k, v) in fields {
        env.insert(format!("{prefix}_{k}"), v.to_value());
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
    cache: &mut GraphCache,
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
                hydrate_invoke_target(engine, cgs, cache, mode, env, invoke, get, prefix).await?;
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
        }
    }
    Ok(())
}

fn env_param_present(env: &CmlEnv, name: &str) -> bool {
    matches!(env.get(name), Some(v) if !matches!(v, Value::Null))
}

#[allow(clippy::too_many_arguments)]
async fn hydrate_invoke_target(
    engine: &ExecutionEngine,
    cgs: &CGS,
    cache: &mut GraphCache,
    mode: ExecutionMode,
    env: &mut CmlEnv,
    invoke: &InvokeExpr,
    get_cap: &str,
    prefix: &str,
) -> Result<(), RuntimeError> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Err(RuntimeError::ConfigurationError {
            message: "preflight hydrate_invoke_target: prefix must not be empty".to_string(),
        });
    }

    if let Some(entity) = cache.get(&invoke.target) {
        if entity.completeness == EntityCompleteness::Complete {
            merge_preflight_fields_into_env(env, prefix, &entity.fields);
            return Ok(());
        }
    }

    let get = GetExpr {
        reference: invoke.target.clone(),
        path_vars: None,
        catalog_entry_id: None,
    };
    let (cached, _source) = engine
        .fetch_get_decoded(&get, cgs, mode, Some(get_cap), false, Some(cache))
        .await?;
    cache.insert(cached.clone())?;
    merge_preflight_fields_into_env(env, prefix, &cached.fields);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn hydrate_entity_ref_param(
    engine: &ExecutionEngine,
    cgs: &CGS,
    cache: &mut GraphCache,
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
        catalog_entry_id: None,
    };
    let (cached, _source) = engine
        .fetch_get_decoded(&get, cgs, mode, Some(get_cap), false, Some(cache))
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
    cache: &mut GraphCache,
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
        .execute_query(&query, cgs, cache, mode, StreamConsumeOpts::default())
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
    cache: &mut GraphCache,
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
    cache: &mut GraphCache,
    mode: ExecutionMode,
    lookup_cap: &CapabilitySchema,
    name: &str,
) -> Result<String, RuntimeError> {
    let mut query = QueryExpr::filtered(lookup_cap.domain.clone(), Predicate::True);
    query.capability_name = Some(lookup_cap.name.clone());
    let res = engine
        .execute_query(&query, cgs, cache, mode, StreamConsumeOpts::default())
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

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::Predicate;

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
