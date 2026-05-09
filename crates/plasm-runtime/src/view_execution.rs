//! CGS `views:` execution — composed reads without dedicated HTTP mappings.

use std::collections::{BTreeMap, HashSet};

use indexmap::IndexMap;
use plasm_core::schema::{EntityDef, ViewOutputBinding, ViewParamBinding};
use plasm_core::{CapabilityKind, GetExpr, Predicate, QueryExpr, Ref, TypedFieldValue, Value, CGS};

use crate::cache::{CachedEntity, EntityCompleteness, GraphCache};
use crate::execution::{
    ExecutionEngine, ExecutionMode, ExecutionResult, ExecutionSource, ExecutionStats,
    StreamConsumeOpts,
};
use crate::RuntimeError;

fn json_to_plasm_value(json: &serde_json::Value) -> Value {
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Null
            }
        }
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Array(arr) => {
            let values = arr.iter().map(json_to_plasm_value).collect();
            Value::Array(values)
        }
        serde_json::Value::Object(obj) => {
            let mut map = IndexMap::new();
            for (k, v) in obj {
                map.insert(k.clone(), json_to_plasm_value(v));
            }
            Value::Object(map)
        }
    }
}

fn predicate_scope_map(predicate: &Predicate) -> Result<IndexMap<String, Value>, RuntimeError> {
    let mut acc: IndexMap<String, Vec<Value>> = IndexMap::new();
    collect_predicate_vars(predicate, &mut acc);
    let mut scope = IndexMap::new();
    for (field, mut values) in acc {
        match values.len() {
            0 => {}
            1 => {
                scope.insert(field, values.remove(0));
            }
            _ => {
                scope.insert(field, Value::Array(values));
            }
        }
    }
    Ok(scope)
}

fn collect_predicate_vars(predicate: &Predicate, acc: &mut IndexMap<String, Vec<Value>>) {
    match predicate {
        Predicate::Comparison { field, op, value } => {
            let rhs = value.to_value();
            match op {
                plasm_core::CompOp::In | plasm_core::CompOp::Contains => match &rhs {
                    Value::Array(arr) => {
                        acc.entry(field.clone())
                            .or_default()
                            .extend(arr.iter().cloned());
                    }
                    other => {
                        acc.entry(field.clone()).or_default().push(other.clone());
                    }
                },
                _ => {
                    acc.entry(field.clone()).or_default().clear();
                    acc.entry(field.clone()).or_default().push(rhs);
                }
            }
        }
        Predicate::And { args } => {
            for arg in args {
                collect_predicate_vars(arg, acc);
            }
        }
        Predicate::Or { args } => {
            for arg in args {
                collect_predicate_vars(arg, acc);
            }
        }
        _ => {}
    }
}

fn resolve_binding(
    binding: &ViewParamBinding,
    scope: &IndexMap<String, Value>,
) -> Result<Value, RuntimeError> {
    match binding {
        ViewParamBinding::Scope { param } => {
            scope
                .get(param)
                .cloned()
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view scope missing `{param}` (from predicate)"),
                })
        }
        ViewParamBinding::Literal { value } => Ok(json_to_plasm_value(value)),
    }
}

fn binds_to_predicate(
    bind: &IndexMap<String, ViewParamBinding>,
    scope: &IndexMap<String, Value>,
) -> Result<Predicate, RuntimeError> {
    let mut args = Vec::new();
    for (param, b) in bind {
        let v = resolve_binding(b, scope)?;
        args.push(Predicate::eq(param.clone(), v));
    }
    Ok(if args.len() == 1 {
        args.pop().expect("one arg")
    } else {
        Predicate::And { args }
    })
}

fn resolve_output_binding(
    binding: &ViewOutputBinding,
    scope: &IndexMap<String, Value>,
    node_results: &IndexMap<String, ExecutionResult>,
) -> Result<Value, RuntimeError> {
    match binding {
        ViewOutputBinding::Scope { param } => Ok(scope.get(param).cloned().unwrap_or(Value::Null)),
        ViewOutputBinding::NodeRowCount { node } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view output references unknown node `{node}`"),
                })?;
            Ok(Value::Integer(r.count as i64))
        }
        ViewOutputBinding::NodeField { node, field } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view output references unknown node `{node}`"),
                })?;
            let Some(row) = r.entities.first() else {
                return Ok(Value::Null);
            };
            Ok(row
                .fields
                .get(field)
                .map(TypedFieldValue::to_value)
                .unwrap_or(Value::Null))
        }
        ViewOutputBinding::NodeFieldHistogramJson { node, field } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view output references unknown node `{node}`"),
                })?;
            Ok(field_histogram_json(&r.entities, field.as_str()))
        }
    }
}

fn field_histogram_json(rows: &[crate::cache::CachedEntity], field: &str) -> Value {
    let mut counts: IndexMap<String, i64> = IndexMap::new();
    for row in rows {
        let k = row
            .fields
            .get(field)
            .map(TypedFieldValue::to_value)
            .map(|v| match v {
                Value::String(s) => s,
                Value::Integer(i) => i.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Float(f) => f.to_string(),
                _ => "<non_scalar>".into(),
            })
            .unwrap_or_else(|| "<missing>".into());
        *counts.entry(k).or_insert(0) += 1;
    }
    let obj: serde_json::Map<String, serde_json::Value> = counts
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::from(v)))
        .collect();
    json_to_plasm_value(&serde_json::Value::Object(obj))
}

fn scalar_string_from_value(v: &Value) -> Result<String, RuntimeError> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Integer(i) => Ok(i.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Float(f) => Ok(f.to_string()),
        _ => Err(RuntimeError::ConfigurationError {
            message: format!("view identity field expected scalar, got {:?}", v),
        }),
    }
}

fn build_view_row_reference(
    view_ent: &EntityDef,
    fields_plain: &IndexMap<String, Value>,
) -> Result<Ref, RuntimeError> {
    let mut parts = BTreeMap::new();
    if !view_ent.key_vars.is_empty() {
        for kv in &view_ent.key_vars {
            let v =
                fields_plain
                    .get(kv.as_str())
                    .ok_or_else(|| RuntimeError::ConfigurationError {
                        message: format!("view output missing key field `{kv}`"),
                    })?;
            parts.insert(kv.to_string(), scalar_string_from_value(v)?);
        }
        Ok(Ref::compound(view_ent.name.clone(), parts))
    } else {
        let idf = view_ent.id_field.as_str();
        let v = fields_plain
            .get(idf)
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!("view output missing id field `{idf}`"),
            })?;
        Ok(Ref::new(
            view_ent.name.clone(),
            scalar_string_from_value(v)?,
        ))
    }
}

/// Run a `views:` composition for an outer [`QueryExpr`] (must target the view entity).
pub(crate) async fn execute_view_query(
    engine: &ExecutionEngine,
    view_name: &str,
    query: &QueryExpr,
    cgs: &CGS,
    cache: &mut GraphCache,
    mode: ExecutionMode,
) -> Result<ExecutionResult, RuntimeError> {
    let Some(view) = cgs.views.get(view_name) else {
        return Err(RuntimeError::ConfigurationError {
            message: format!("unknown composed view `{view_name}`"),
        });
    };

    let view_entity =
        cgs.get_entity(&view.entity)
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!(
                    "view `{}` targets unknown entity {}",
                    view_name, view.entity
                ),
            })?;

    if query.entity.as_str() != view.entity.as_str() {
        return Err(RuntimeError::ConfigurationError {
            message: format!(
                "view `{view_name}` targets entity {} but query was for {}",
                view.entity.as_str(),
                query.entity.as_str()
            ),
        });
    }

    let pred = query
        .predicate
        .as_ref()
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!(
                "view `{view_name}` requires a query predicate supplying scope parameters"
            ),
        })?;

    let scope = predicate_scope_map(pred)?;

    let mut expected_scope = HashSet::new();
    for s in &view.scope {
        expected_scope.insert(s.name.as_str());
    }
    if !expected_scope.is_empty() {
        for name in &expected_scope {
            if !scope.contains_key(*name) {
                return Err(RuntimeError::ConfigurationError {
                    message: format!(
                        "view `{view_name}` requires predicate field `{name}` (declared under views.scope)"
                    ),
                });
            }
        }
    }

    let mut node_results: IndexMap<String, ExecutionResult> = IndexMap::new();
    let mut stats = ExecutionStats {
        duration_ms: 0,
        network_requests: 0,
        cache_hits: 0,
        cache_misses: 0,
    };
    let mut fingerprints: Vec<String> = Vec::new();

    for node in &view.nodes {
        let cap = cgs
            .get_capability(node.capability.as_str())
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: node.capability.clone(),
                entity: query.entity.to_string(),
            })?;

        match cap.kind {
            CapabilityKind::Query => {
                let pred_node = binds_to_predicate(&node.bind, &scope)?;
                let q = QueryExpr::filtered(cap.domain.clone(), pred_node);
                let res = engine
                    .execute_query(&q, cgs, cache, mode, StreamConsumeOpts::default())
                    .await?;
                stats.network_requests += res.stats.network_requests;
                stats.cache_hits += res.stats.cache_hits;
                stats.cache_misses += res.stats.cache_misses;
                fingerprints.extend(res.request_fingerprints.iter().cloned());
                node_results.insert(node.id.clone(), res);
            }
            CapabilityKind::Get => {
                let id_val = if node.bind.len() == 1 {
                    let (_k, b) = node.bind.iter().next().expect("one bind");
                    resolve_binding(b, &scope)?
                } else {
                    return Err(RuntimeError::ConfigurationError {
                        message: format!(
                            "view node `{}`: Get capability `{}` expects exactly one binding (the id)",
                            node.id, node.capability
                        ),
                    });
                };
                let id_str = match id_val {
                    Value::String(s) => s,
                    Value::Integer(i) => i.to_string(),
                    other => {
                        return Err(RuntimeError::ConfigurationError {
                            message: format!(
                                "view node `{}`: Get id must be string-like, got {:?}",
                                node.id, other
                            ),
                        });
                    }
                };
                let get = GetExpr::new(cap.domain.clone(), id_str);
                let res = engine.execute_get(&get, cgs, cache, mode).await?;
                stats.network_requests += res.stats.network_requests;
                stats.cache_hits += res.stats.cache_hits;
                stats.cache_misses += res.stats.cache_misses;
                fingerprints.extend(res.request_fingerprints.iter().cloned());
                node_results.insert(node.id.clone(), res);
            }
            _ => {
                return Err(RuntimeError::ConfigurationError {
                    message: format!(
                        "view node `{}`: unsupported capability kind {:?}",
                        node.id, cap.kind
                    ),
                });
            }
        }
    }

    let mut fields_plain: IndexMap<String, Value> = IndexMap::new();
    for (fname, binding) in &view.output {
        let v = resolve_output_binding(binding, &scope, &node_results)?;
        fields_plain.insert(fname.clone(), v);
    }

    let reference = build_view_row_reference(view_entity, &fields_plain)?;

    let ts = crate::execution::current_timestamp();
    let cached = CachedEntity::from_decoded(
        reference,
        fields_plain,
        IndexMap::new(),
        ts,
        EntityCompleteness::Complete,
    );

    Ok(ExecutionResult {
        entities: vec![cached],
        count: 1,
        has_more: false,
        pagination_resume: None,
        paging_handle: None,
        source: ExecutionSource::Live,
        stats,
        request_fingerprints: fingerprints,
    })
}
