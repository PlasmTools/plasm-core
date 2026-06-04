//! CGS `views:` execution — composed reads without dedicated HTTP mappings.
//!
//! Execution phases follow the internal [`crate::view_typestate`] sketch: bind scope → run nodes →
//! materialize agent-facing row + relation refs (no `node_*` vocabulary in prompts).

use std::collections::BTreeMap;

use indexmap::IndexMap;
use plasm_compile::DecodedRelation;
use plasm_core::expr::EntityKey;
use plasm_core::schema::{
    EntityDef, ViewOutputBinding, ViewParamBinding, ViewRelationBinding, ViewScopeInject,
};
use plasm_core::{CapabilityKind, GetExpr, Predicate, QueryExpr, Ref, TypedFieldValue, Value, CGS};

use crate::cache::{CachedEntity, EntityCompleteness};
use crate::execution::{
    ExecutionEngine, ExecutionMode, ExecutionResult, ExecutionSource, ExecutionStats,
    StreamConsumeOpts,
};
use crate::materialization::SessionMaterialization;
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

fn scope_from_get_reference(
    view_ent: &EntityDef,
    get: &GetExpr,
) -> Result<IndexMap<String, Value>, RuntimeError> {
    let mut scope = IndexMap::new();
    match &get.reference.key {
        EntityKey::Simple(id) => {
            scope.insert(
                view_ent.id_field.to_string(),
                Value::String(id.as_str().to_string()),
            );
        }
        EntityKey::Compound(parts) => {
            for (k, v) in parts {
                scope.insert(k.clone(), Value::String(v.clone()));
            }
        }
    }
    Ok(scope)
}

fn validate_expected_scope(
    view_name: &str,
    view: &plasm_core::schema::ViewDefinition,
    scope: &IndexMap<String, Value>,
) -> Result<(), RuntimeError> {
    for sp in &view.scope {
        if !sp.required {
            continue;
        }
        if !scope.contains_key(sp.name.as_str()) {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "view `{view_name}` requires identity/scope field `{}` (declared under views.scope)",
                    sp.name
                ),
            });
        }
    }
    Ok(())
}

/// First-row field snapshots from prior view DAG nodes (for param bind resolution).
type ViewNodeFieldMap = IndexMap<String, IndexMap<String, Value>>;

fn node_fields_from_results(node_results: &IndexMap<String, ExecutionResult>) -> ViewNodeFieldMap {
    let mut out: ViewNodeFieldMap = IndexMap::new();
    for (node_id, res) in node_results {
        let Some(row) = res.entities.first() else {
            out.insert(node_id.clone(), IndexMap::new());
            continue;
        };
        let mut fields = IndexMap::new();
        for (k, v) in &row.fields {
            fields.insert(k.clone(), v.to_value());
        }
        out.insert(node_id.clone(), fields);
    }
    out
}

fn merge_view_ambient_scope(
    view: &plasm_core::schema::ViewDefinition,
    scope: &mut IndexMap<String, Value>,
) {
    let material = crate::execution::try_current_execute_session_material();
    let http_base = crate::execution::try_current_http_base_string();

    let transport = material
        .as_ref()
        .and_then(|m| m.transport_origin.clone())
        .or_else(|| http_base.clone());
    let ui = material
        .as_ref()
        .and_then(|m| m.ui_origin.clone())
        .or_else(|| transport.clone());

    for sp in &view.scope {
        let Some(inject) = sp.inject else {
            continue;
        };
        if scope.contains_key(sp.name.as_str()) {
            continue;
        }
        let origin = match inject {
            ViewScopeInject::SessionUiOrigin => ui.as_ref(),
            ViewScopeInject::SessionTransportOrigin => transport.as_ref(),
        };
        if let Some(o) = origin.filter(|s| !s.trim().is_empty()) {
            scope.insert(
                sp.name.clone(),
                Value::String(o.trim_end_matches('/').to_string()),
            );
        }
    }
}

fn resolve_binding(
    binding: &ViewParamBinding,
    scope: &IndexMap<String, Value>,
    node_fields: &ViewNodeFieldMap,
) -> Result<Value, RuntimeError> {
    match binding {
        ViewParamBinding::Scope { param } => {
            scope
                .get(param)
                .cloned()
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view scope missing `{param}`"),
                })
        }
        ViewParamBinding::Literal { value } => Ok(json_to_plasm_value(value)),
        ViewParamBinding::NodeField { node, field } => {
            let fields = node_fields
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view bind references unknown node `{node}`"),
                })?;
            Ok(fields.get(field).cloned().unwrap_or(Value::Null))
        }
        ViewParamBinding::Computed { template } => {
            crate::view_template::render_view_param_bind_template(template, scope, node_fields)
        }
    }
}

fn binds_to_predicate(
    bind: &IndexMap<String, ViewParamBinding>,
    scope: &IndexMap<String, Value>,
    node_fields: &ViewNodeFieldMap,
) -> Result<Predicate, RuntimeError> {
    let mut args = Vec::new();
    for (param, b) in bind {
        let v = resolve_binding(b, scope, node_fields)?;
        args.push(Predicate::eq(param.clone(), v));
    }
    Ok(if args.len() == 1 {
        args.pop().expect("one arg")
    } else {
        Predicate::And { args }
    })
}

fn values_semantically_equal(row_val: &Value, expected_json: &serde_json::Value) -> bool {
    let expected = json_to_plasm_value(expected_json);
    row_val == &expected
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
        ViewOutputBinding::NodeAnyRowFieldEquals {
            node,
            field,
            equals,
        } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view output references unknown node `{node}`"),
                })?;
            let hit = r.entities.iter().any(|row| {
                let v = row
                    .fields
                    .get(field)
                    .map(TypedFieldValue::to_value)
                    .unwrap_or(Value::Null);
                values_semantically_equal(&v, equals)
            });
            Ok(Value::Bool(hit))
        }
        ViewOutputBinding::NodeRowCountPositive { node } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view output references unknown node `{node}`"),
                })?;
            Ok(Value::Bool(r.count > 0))
        }
        ViewOutputBinding::Computed { .. } => Err(RuntimeError::ConfigurationError {
            message: "computed output bindings are resolved in a separate phase".into(),
        }),
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
        Value::Null => Ok(String::new()),
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

fn bound_scalar_for_get_param(
    param: &str,
    cap: &plasm_core::CapabilitySchema,
    bound_param_to_string: &BTreeMap<String, String>,
) -> Option<String> {
    if let Some(v) = bound_param_to_string.get(param) {
        return Some(v.clone());
    }
    if let Some(fields) = cap.object_params() {
        let required: Vec<_> = fields.iter().filter(|f| f.required).collect();
        if required.len() == 1 && required[0].name == param && bound_param_to_string.len() == 1 {
            return bound_param_to_string.values().next().cloned();
        }
        for f in fields {
            if f.name == param {
                if let Some(v) = bound_param_to_string.get(f.name.as_str()) {
                    return Some(v.clone());
                }
            }
        }
    }
    bound_param_to_string.get("id").cloned()
}

fn ref_from_get_bind_params(
    target_ent: &EntityDef,
    cap: &plasm_core::CapabilitySchema,
    bound_param_to_string: &BTreeMap<String, String>,
) -> Result<Ref, RuntimeError> {
    if !target_ent.key_vars.is_empty() {
        let mut parts = BTreeMap::new();
        for kv in &target_ent.key_vars {
            let s = bound_scalar_for_get_param(kv.as_str(), cap, bound_param_to_string)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!(
                        "view Get node: missing binding for parameter `{}` (needed for `{}` key_vars)",
                        kv, target_ent.name
                    ),
                })?;
            parts.insert(kv.to_string(), s);
        }
        Ok(Ref::compound(target_ent.name.clone(), parts))
    } else {
        let idf = target_ent.id_field.as_str();
        let id = bound_scalar_for_get_param(idf, cap, bound_param_to_string).ok_or_else(|| {
            RuntimeError::ConfigurationError {
                message: format!("view Get node: missing binding for id parameter `{}`", idf),
            }
        })?;
        Ok(Ref::new(target_ent.name.clone(), id))
    }
}

/// View DAG Get nodes for parameterless singleton reads (e.g. Linear `user_viewer`).
fn ref_from_view_get_node(
    target_ent: &EntityDef,
    cap: &plasm_core::CapabilitySchema,
    bound_param_to_string: &BTreeMap<String, String>,
) -> Result<Ref, RuntimeError> {
    if bound_param_to_string.is_empty() {
        let required = cap
            .object_params()
            .map(|fields| fields.iter().any(|f| f.required))
            .unwrap_or(false);
        if !required {
            return Ok(Ref::new(target_ent.name.clone(), String::new()));
        }
    }
    ref_from_get_bind_params(target_ent, cap, bound_param_to_string)
}

fn cached_row_to_target_ref(
    target_ent: &EntityDef,
    row: &CachedEntity,
) -> Result<Ref, RuntimeError> {
    let mut parts = BTreeMap::new();
    if !target_ent.key_vars.is_empty() {
        for kv in &target_ent.key_vars {
            let v = row
                .fields
                .get(kv.as_str())
                .map(TypedFieldValue::to_value)
                .unwrap_or(Value::Null);
            parts.insert(kv.to_string(), scalar_string_from_value(&v)?);
        }
        Ok(Ref::compound(target_ent.name.clone(), parts))
    } else {
        let v = row
            .fields
            .get(target_ent.id_field.as_str())
            .map(TypedFieldValue::to_value)
            .unwrap_or(Value::Null);
        Ok(Ref::new(
            target_ent.name.clone(),
            scalar_string_from_value(&v)?,
        ))
    }
}

fn rows_for_binding<'a>(
    binding: &'a ViewRelationBinding,
    node_results: &'a IndexMap<String, ExecutionResult>,
) -> Result<Vec<&'a CachedEntity>, RuntimeError> {
    match binding {
        ViewRelationBinding::FirstNodeRowWhere {
            node,
            where_field,
            equals,
        }
        | ViewRelationBinding::NodeRowsWhere {
            node,
            where_field,
            equals,
        } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view relation_output references unknown node `{node}`"),
                })?;
            let matched: Vec<&CachedEntity> = r
                .entities
                .iter()
                .filter(|row| {
                    let v = row
                        .fields
                        .get(where_field.as_str())
                        .map(TypedFieldValue::to_value)
                        .unwrap_or(Value::Null);
                    values_semantically_equal(&v, equals)
                })
                .collect();
            Ok(matched)
        }
        ViewRelationBinding::NodeAllRows { node } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view relation_output references unknown node `{node}`"),
                })?;
            Ok(r.entities.iter().collect())
        }
        ViewRelationBinding::NodeSingleRow { node } => {
            let r = node_results
                .get(node)
                .ok_or_else(|| RuntimeError::ConfigurationError {
                    message: format!("view relation_output references unknown node `{node}`"),
                })?;
            Ok(r.entities.iter().collect::<Vec<_>>())
        }
    }
}

fn resolve_view_relation_maps(
    view: &plasm_core::schema::ViewDefinition,
    node_results: &IndexMap<String, ExecutionResult>,
    cgs: &CGS,
) -> Result<IndexMap<String, DecodedRelation>, RuntimeError> {
    let mut out: IndexMap<String, DecodedRelation> = IndexMap::new();
    for spec in &view.relation_outputs {
        let target_ent = cgs.get_entity(spec.target.as_str()).ok_or_else(|| {
            RuntimeError::ConfigurationError {
                message: format!(
                    "view relation_output references unknown target entity `{}`",
                    spec.target
                ),
            }
        })?;
        let refs: Vec<Ref> = match &spec.binding {
            ViewRelationBinding::FirstNodeRowWhere { .. } => {
                let rows = rows_for_binding(&spec.binding, node_results)?;
                if let Some(row) = rows.first() {
                    vec![cached_row_to_target_ref(target_ent, row)?]
                } else {
                    Vec::new()
                }
            }
            ViewRelationBinding::NodeRowsWhere { .. } | ViewRelationBinding::NodeAllRows { .. } => {
                let rows = rows_for_binding(&spec.binding, node_results)?;
                rows.into_iter()
                    .map(|row| cached_row_to_target_ref(target_ent, row))
                    .collect::<Result<Vec<_>, _>>()?
            }
            ViewRelationBinding::NodeSingleRow { node } => {
                let r = node_results
                    .get(node)
                    .ok_or_else(|| RuntimeError::ConfigurationError {
                        message: format!("view relation_output references unknown node `{node}`"),
                    })?;
                if r.count != 1 {
                    return Err(RuntimeError::ConfigurationError {
                        message: format!(
                            "view relation_output node_single_row `{node}` expected exactly one entity (got {})",
                            r.count
                        ),
                    });
                }
                let row = r
                    .entities
                    .first()
                    .ok_or_else(|| RuntimeError::ConfigurationError {
                        message: format!("view relation_output node `{node}` missing row"),
                    })?;
                vec![cached_row_to_target_ref(target_ent, row)?]
            }
        };
        if !refs.is_empty() {
            out.insert(spec.relation.to_string(), DecodedRelation::Specified(refs));
        }
    }
    Ok(out)
}

async fn execute_view_scoped(
    engine: &ExecutionEngine,
    view_name: &str,
    mut scope: IndexMap<String, Value>,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
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

    merge_view_ambient_scope(view, &mut scope);
    validate_expected_scope(view_name, view, &scope)?;

    let mut node_results: IndexMap<String, ExecutionResult> = IndexMap::new();
    let mut stats = ExecutionStats {
        duration_ms: 0,
        network_requests: 0,
        cache_hits: 0,
        cache_misses: 0,
        ..Default::default()
    };
    let mut fingerprints: Vec<String> = Vec::new();
    let mut any_live = false;

    for node in &view.nodes {
        let node_fields = node_fields_from_results(&node_results);
        let cap = cgs
            .get_capability(node.capability.as_str())
            .ok_or_else(|| RuntimeError::CapabilityNotFound {
                capability: node.capability.clone(),
                entity: view.entity.to_string(),
            })?;

        match cap.kind {
            CapabilityKind::Query | CapabilityKind::Search => {
                let pred_node = binds_to_predicate(&node.bind, &scope, &node_fields)?;
                let q = QueryExpr::filtered(cap.domain.clone(), pred_node);
                let res = engine
                    .execute_query(&q, cgs, cache, mode, StreamConsumeOpts::default())
                    .await?;
                if res.source == ExecutionSource::Live {
                    any_live = true;
                }
                stats.network_requests += res.stats.network_requests;
                stats.cache_hits += res.stats.cache_hits;
                stats.cache_misses += res.stats.cache_misses;
                fingerprints.extend(res.request_fingerprints.iter().cloned());
                node_results.insert(node.id.clone(), res);
            }
            CapabilityKind::Get => {
                let mut bound = BTreeMap::new();
                for (param, bspec) in &node.bind {
                    let v = resolve_binding(bspec, &scope, &node_fields)?;
                    bound.insert(param.clone(), scalar_string_from_value(&v)?);
                }
                let target_ent = cgs.get_entity(cap.domain.as_str()).ok_or_else(|| {
                    RuntimeError::ConfigurationError {
                        message: format!(
                            "view node `{}`: unknown entity domain `{}`",
                            node.id, cap.domain
                        ),
                    }
                })?;
                let reference = ref_from_view_get_node(target_ent, cap, &bound)?;
                let get = GetExpr::from_ref(reference);
                let res = engine
                    .execute_get_for_view_dag(&get, cgs, cache, mode)
                    .await?;
                if res.source == ExecutionSource::Live {
                    any_live = true;
                }
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
        if matches!(binding, ViewOutputBinding::Computed { .. }) {
            continue;
        }
        let v = resolve_output_binding(binding, &scope, &node_results)?;
        fields_plain.insert(fname.clone(), v);
    }
    for (fname, binding) in &view.output {
        let ViewOutputBinding::Computed { template } = binding else {
            continue;
        };
        let v =
            crate::view_template::render_view_computed_template(template, &scope, &fields_plain)?;
        fields_plain.insert(fname.clone(), v);
    }

    let reference = build_view_row_reference(view_entity, &fields_plain)?;

    let relation_decoded = resolve_view_relation_maps(view, &node_results, cgs)?;
    let ts = crate::execution::current_timestamp();
    let cached = CachedEntity::from_decoded(
        reference,
        fields_plain,
        relation_decoded,
        ts,
        EntityCompleteness::Complete,
    );

    Ok(ExecutionResult {
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
    })
}

/// Run a `views:` composition for an outer [`QueryExpr`] (must target the view entity).
pub(crate) async fn execute_view_query(
    engine: &ExecutionEngine,
    view_name: &str,
    query: &QueryExpr,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
) -> Result<ExecutionResult, RuntimeError> {
    let Some(view) = cgs.views.get(view_name) else {
        return Err(RuntimeError::ConfigurationError {
            message: format!("unknown composed view `{view_name}`"),
        });
    };

    if query.entity.as_str() != view.entity.as_str() {
        return Err(RuntimeError::ConfigurationError {
            message: format!(
                "view `{view_name}` targets entity {} but query was for {}",
                view.entity.as_str(),
                query.entity.as_str()
            ),
        });
    }

    let scope = match &query.predicate {
        Some(pred) => predicate_scope_map(pred)?,
        None if view.scope.iter().all(|s| !s.required || s.inject.is_some()) => IndexMap::new(),
        None => {
            return Err(RuntimeError::ConfigurationError {
                message: format!(
                    "view `{view_name}` requires a query predicate supplying scope parameters"
                ),
            });
        }
    };
    execute_view_scoped(engine, view_name, scope, cgs, cache, mode).await
}

/// Run a `views:` composition for an outer [`GetExpr`] on the view entity.
pub(crate) async fn execute_view_get(
    engine: &ExecutionEngine,
    view_name: &str,
    get: &GetExpr,
    cgs: &CGS,
    cache: &mut SessionMaterialization,
    mode: ExecutionMode,
) -> Result<ExecutionResult, RuntimeError> {
    let Some(view) = cgs.views.get(view_name) else {
        return Err(RuntimeError::ConfigurationError {
            message: format!("unknown composed view `{view_name}`"),
        });
    };

    if get.reference.entity_type.as_str() != view.entity.as_str() {
        return Err(RuntimeError::ConfigurationError {
            message: format!(
                "view `{view_name}` targets entity {} but get ref was for {}",
                view.entity.as_str(),
                get.reference.entity_type
            ),
        });
    }

    let view_entity =
        cgs.get_entity(&view.entity)
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!(
                    "view `{}` targets unknown entity {}",
                    view_name, view.entity
                ),
            })?;

    let scope = scope_from_get_reference(view_entity, get)?;
    execute_view_scoped(engine, view_name, scope, cgs, cache, mode).await
}
