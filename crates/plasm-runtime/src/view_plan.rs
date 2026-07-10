//! Shared view DAG orchestration — live, preflight, and fixture runners share this path.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use plasm_compile::DecodedRelation;
use plasm_core::expr::EntityKey;
use plasm_core::schema::{
    EntityDef, ViewDefinition, ViewNodeSpec, ViewOutputBinding, ViewParamBinding,
    ViewRelationBinding, ViewScopeInject,
};
use plasm_core::{
    CapabilityKind, CapabilitySchema, CreateExpr, GetExpr, Predicate, QueryExpr, Ref,
    TypedFieldValue, Value, ViewNodeCondition, ViewNodeWhen, WriteOutcome, CGS,
};

use crate::cache::CachedEntity;
use crate::execution::{ExecutionResult, ExecutionSource, ExecutionStats};

use crate::RuntimeError;

/// Session transport / UI origins for inject-only view scope params.
///
/// Thread explicitly at dispatch boundaries — [`crate::execution::ExecuteOptions::view_ambient`]
/// (live execute, from pinned [`crate::execution::ExecuteSessionMaterial`]), the agent host's
/// `ExecuteSession::view_ambient` (dry preflight), or [`ViewAmbientContext::default`] in unit
/// tests. No task-local lookup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewAmbientContext {
    pub transport_origin: Option<String>,
    pub ui_origin: Option<String>,
}

impl ViewAmbientContext {
    /// Build from pinned execute session material at an explicit dispatch boundary.
    pub fn from_execute_material(material: &crate::execution::ExecuteSessionMaterial) -> Self {
        Self::from(material)
    }

    /// Build from an optional HTTP backend when no full session material is available.
    pub fn from_http_backend(backend: Option<&str>) -> Self {
        let Some(base) = backend.map(str::trim).filter(|s| !s.is_empty()) else {
            return Self::default();
        };
        Self {
            transport_origin: Some(base.to_string()),
            ui_origin: Some(base.to_string()),
        }
    }
}

impl From<&crate::execution::ExecuteSessionMaterial> for ViewAmbientContext {
    fn from(material: &crate::execution::ExecuteSessionMaterial) -> Self {
        let transport = material.transport_origin.clone();
        Self {
            ui_origin: material.ui_origin.clone().or_else(|| transport.clone()),
            transport_origin: transport,
        }
    }
}

#[allow(dead_code)] // `scope` is read today; other fields are the runner contract surface.
pub(crate) struct ViewRunContext<'a> {
    pub view_name: &'a str,
    pub scope: &'a IndexMap<String, Value>,
    pub cgs: &'a CGS,
    pub ambient: &'a ViewAmbientContext,
}

/// Structured observable from one view DAG walk (stub or live node I/O).
#[derive(Debug, Clone)]
pub struct ViewRunProof {
    pub scope: IndexMap<String, Value>,
    pub node_results: IndexMap<String, ExecutionResult>,
    pub output_fields: IndexMap<String, Value>,
    pub relation_refs: IndexMap<String, DecodedRelation>,
    pub row_ref: Ref,
    pub stats: ExecutionStats,
    pub request_fingerprints: Vec<String>,
    pub any_live: bool,
}

/// Executes one view DAG node synchronously (preflight / fixture runners).
pub(crate) trait ViewNodeRunner {
    fn run_query_node(
        &self,
        ctx: &ViewRunContext<'_>,
        node: &ViewNodeSpec,
        cap: &CapabilitySchema,
        pred: &Predicate,
        node_fields: &ViewNodeFieldMap,
    ) -> Result<ExecutionResult, RuntimeError>;

    fn run_get_node(
        &self,
        ctx: &ViewRunContext<'_>,
        node: &ViewNodeSpec,
        cap: &CapabilitySchema,
        get: &GetExpr,
        bound: &BTreeMap<String, String>,
    ) -> Result<ExecutionResult, RuntimeError>;

    fn run_create_node(
        &self,
        ctx: &ViewRunContext<'_>,
        node: &ViewNodeSpec,
        cap: &CapabilitySchema,
        create: &CreateExpr,
    ) -> Result<ExecutionResult, RuntimeError>;
}

/// Executes one view DAG node asynchronously (live HTTP).
#[async_trait::async_trait]
pub(crate) trait ViewNodeRunnerAsync {
    async fn run_query_node(
        &mut self,
        ctx: &ViewRunContext<'_>,
        node: &ViewNodeSpec,
        cap: &CapabilitySchema,
        pred: &Predicate,
        node_fields: &ViewNodeFieldMap,
    ) -> Result<ExecutionResult, RuntimeError>;

    async fn run_get_node(
        &mut self,
        ctx: &ViewRunContext<'_>,
        node: &ViewNodeSpec,
        cap: &CapabilitySchema,
        get: &GetExpr,
        bound: &BTreeMap<String, String>,
    ) -> Result<ExecutionResult, RuntimeError>;

    async fn run_create_node(
        &mut self,
        ctx: &ViewRunContext<'_>,
        node: &ViewNodeSpec,
        cap: &CapabilitySchema,
        create: &CreateExpr,
    ) -> Result<ExecutionResult, RuntimeError>;
}

/// First-row field snapshots from prior view DAG nodes (for param bind resolution).
pub type ViewNodeFieldMap = IndexMap<String, IndexMap<String, Value>>;

pub(crate) fn json_to_plasm_value(json: &serde_json::Value) -> Value {
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

pub fn view_node_should_run(
    when: Option<&ViewNodeWhen>,
    node_results: &IndexMap<String, ExecutionResult>,
) -> bool {
    let Some(when) = when else {
        return true;
    };
    match when {
        ViewNodeWhen::SkipIf { condition } => !eval_view_condition(condition, node_results),
        ViewNodeWhen::RunIf { condition } => eval_view_condition(condition, node_results),
    }
}

fn eval_view_condition(
    condition: &ViewNodeCondition,
    node_results: &IndexMap<String, ExecutionResult>,
) -> bool {
    match condition {
        ViewNodeCondition::NodeRowCountPositive { node } => node_results
            .get(node)
            .is_some_and(|r| r.count > 0),
        ViewNodeCondition::NodeRowCountZero { node } => node_results
            .get(node)
            .is_none_or(|r| r.count == 0),
    }
}

pub fn predicate_scope_map(predicate: &Predicate) -> Result<IndexMap<String, Value>, RuntimeError> {
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

pub fn scope_from_get_reference(
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

pub fn validate_expected_scope(
    view_name: &str,
    view: &ViewDefinition,
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

#[allow(dead_code)] // public helper for recomputing binds from prior node results
pub fn node_fields_from_results(
    node_results: &IndexMap<String, ExecutionResult>,
) -> ViewNodeFieldMap {
    node_results
        .iter()
        .map(|(node_id, res)| (node_id.clone(), node_fields_for_row(res.entities.first())))
        .collect()
}

pub(crate) fn node_fields_for_row(row: Option<&CachedEntity>) -> IndexMap<String, Value> {
    let Some(row) = row else {
        return IndexMap::new();
    };
    row.fields
        .iter()
        .map(|(k, v)| (k.clone(), v.to_value()))
        .collect()
}

pub fn merge_view_ambient_scope(
    view: &ViewDefinition,
    scope: &mut IndexMap<String, Value>,
    ambient: &ViewAmbientContext,
) {
    let transport = ambient.transport_origin.as_deref();
    let ui = ambient.ui_origin.as_deref().or(transport);

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

pub fn resolve_binding(
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

pub fn binds_to_predicate(
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

pub fn resolve_output_binding(
    binding: &ViewOutputBinding,
    scope: &IndexMap<String, Value>,
    node_results: &IndexMap<String, ExecutionResult>,
    write_outcomes: &IndexMap<String, WriteOutcome>,
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
        ViewOutputBinding::WriteCreated { node } => Ok(Value::Bool(
            matches!(write_outcomes.get(node), Some(WriteOutcome::Created)),
        )),
        ViewOutputBinding::WriteReused { node } => Ok(Value::Bool(
            matches!(write_outcomes.get(node), Some(WriteOutcome::Reused)),
        )),
        ViewOutputBinding::WriteSkipped { node } => Ok(Value::Bool(
            matches!(write_outcomes.get(node), Some(WriteOutcome::Skipped)),
        )),
        ViewOutputBinding::Computed { .. } => Err(RuntimeError::ConfigurationError {
            message: "computed output bindings are resolved in a separate phase".into(),
        }),
    }
}

fn field_histogram_json(rows: &[CachedEntity], field: &str) -> Value {
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

pub fn scalar_string_from_value(v: &Value) -> Result<String, RuntimeError> {
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

pub fn build_view_row_reference(
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
    cap: &CapabilitySchema,
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

pub fn ref_from_get_bind_params(
    target_ent: &EntityDef,
    cap: &CapabilitySchema,
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

pub fn ref_from_view_get_node(
    target_ent: &EntityDef,
    cap: &CapabilitySchema,
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

pub fn resolve_view_relation_maps(
    view: &ViewDefinition,
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

/// Scope for an outer view query (predicate → scope map).
pub(crate) fn derive_view_query_scope(
    view_name: &str,
    query: &QueryExpr,
    cgs: &CGS,
) -> Result<IndexMap<String, Value>, RuntimeError> {
    let view = cgs
        .views
        .get(view_name)
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!("unknown composed view `{view_name}`"),
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
    match &query.predicate {
        Some(pred) => predicate_scope_map(pred),
        None if view.scope.iter().all(|s| !s.required || s.inject.is_some()) => Ok(IndexMap::new()),
        None => Err(RuntimeError::ConfigurationError {
            message: format!(
                "view `{view_name}` requires a query predicate supplying scope parameters"
            ),
        }),
    }
}

/// Scope for an outer view get (reference key → scope map).
pub(crate) fn derive_view_get_scope(
    view_name: &str,
    get: &GetExpr,
    cgs: &CGS,
) -> Result<IndexMap<String, Value>, RuntimeError> {
    let view = cgs
        .views
        .get(view_name)
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!("unknown composed view `{view_name}`"),
        })?;
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
                message: format!("view `{view_name}` targets unknown entity {}", view.entity),
            })?;
    scope_from_get_reference(view_entity, get)
}

/// Prepared inner capability invocation for one view DAG node.
pub(crate) enum PreparedViewNode<'a> {
    Query {
        cap: &'a CapabilitySchema,
        pred: Predicate,
    },
    Get {
        cap: &'a CapabilitySchema,
        get: GetExpr,
        bound: BTreeMap<String, String>,
    },
    Create {
        cap: &'a CapabilitySchema,
        create: CreateExpr,
    },
}

pub(crate) fn prepare_view_node<'a>(
    node: &ViewNodeSpec,
    scope: &IndexMap<String, Value>,
    node_fields: &ViewNodeFieldMap,
    cgs: &'a CGS,
    view_entity: &str,
) -> Result<PreparedViewNode<'a>, RuntimeError> {
    let cap = cgs
        .get_capability(node.capability.as_str())
        .ok_or_else(|| RuntimeError::CapabilityNotFound {
            capability: node.capability.clone(),
            entity: view_entity.to_string(),
        })?;
    match cap.kind {
        CapabilityKind::Query | CapabilityKind::Search => {
            let pred = binds_to_predicate(&node.bind, scope, node_fields)?;
            Ok(PreparedViewNode::Query { cap, pred })
        }
        CapabilityKind::Get => {
            let mut bound = BTreeMap::new();
            for (param, bspec) in &node.bind {
                let v = resolve_binding(bspec, scope, node_fields)?;
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
            Ok(PreparedViewNode::Get {
                cap,
                get: GetExpr::from_ref(reference),
                bound,
            })
        }
        CapabilityKind::Create | CapabilityKind::Action => {
            let mut input_map = IndexMap::new();
            for (param, bspec) in &node.bind {
                let v = resolve_binding(bspec, scope, node_fields)?;
                input_map.insert(param.clone(), v);
            }
            let create = CreateExpr {
                capability: cap.name.clone(),
                entity: cap.domain.clone(),
                input: Value::Object(input_map).into(),
                catalog_entry_id: Default::default(),
                dotted_receiver: None,
            };
            Ok(PreparedViewNode::Create { cap, create })
        }
        other => Err(RuntimeError::ConfigurationError {
            message: format!(
                "view node `{}`: unsupported capability kind {other:?}",
                node.id
            ),
        }),
    }
}

pub(crate) fn load_view_dag<'a>(
    view_name: &str,
    mut scope: IndexMap<String, Value>,
    cgs: &'a CGS,
    ambient: &ViewAmbientContext,
) -> Result<(&'a ViewDefinition, &'a EntityDef, IndexMap<String, Value>), RuntimeError> {
    let view = cgs
        .views
        .get(view_name)
        .ok_or_else(|| RuntimeError::ConfigurationError {
            message: format!("unknown composed view `{view_name}`"),
        })?;
    let view_entity =
        cgs.get_entity(&view.entity)
            .ok_or_else(|| RuntimeError::ConfigurationError {
                message: format!("view `{view_name}` targets unknown entity {}", view.entity),
            })?;
    merge_view_ambient_scope(view, &mut scope, ambient);
    validate_expected_scope(view_name, view, &scope)?;
    Ok((view, view_entity, scope))
}

/// Accumulate node execution stats from one node result.
pub(crate) fn absorb_node_stats(
    stats: &mut ExecutionStats,
    fingerprints: &mut Vec<String>,
    any_live: &mut bool,
    res: &ExecutionResult,
) {
    if res.source == ExecutionSource::Live {
        *any_live = true;
    }
    stats.network_requests += res.stats.network_requests;
    stats.cache_hits += res.stats.cache_hits;
    stats.cache_misses += res.stats.cache_misses;
    fingerprints.extend(res.request_fingerprints.iter().cloned());
}

#[cfg(test)]
#[path = "view_plan_tests.rs"]
mod tests;
