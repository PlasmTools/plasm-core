//! RA-9: password-domain `value_ref` cells may only fill the same named value.

use super::prelude::*;
use super::schema_validate::cgs_for_qualified_entity;
use super::types::{CompileState, DagNodeSource};
use plasm_core::schema::InputFieldWire;
use plasm_core::{
    CreateExpr, PlasmInputRef, Predicate, QueryExpr, Value, ValueDomainSlot, WithExpr,
};

const PASSWORD_EXACT: &str = "nv_password";
const PASSWORD_SUFFIX: &str = "_password";

/// Reject password-domain `PlasmInputRef` fills into a different named value (RA-9).
pub(in crate::plasm_dag) fn validate_password_domain_bind(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    expr: &Expr,
) -> Result<(), String> {
    match expr {
        Expr::Query(q) => validate_query_selection(session, state, node_id, q),
        Expr::Invoke(inv) => validate_targeted(
            session,
            state,
            node_id,
            inv,
            inv.catalog_entry_id.as_deref(),
        ),
        Expr::Delete(delete) => validate_targeted(
            session,
            state,
            node_id,
            delete,
            delete.catalog_entry_id.as_deref(),
        ),
        Expr::Create(c) => validate_create(session, state, node_id, c),
        Expr::Chain(chain) => {
            validate_password_domain_bind(session, state, node_id, &chain.source)?;
            if let plasm_core::ChainStep::Explicit { expr } = &chain.step {
                validate_password_domain_bind(session, state, node_id, expr)?;
            }
            Ok(())
        }
        Expr::Get(_)
        | Expr::Page(_)
        | Expr::Wait(_)
        | Expr::Cancel(_)
        | Expr::TeachingValue { .. } => Ok(()),
    }
}

fn validate_query_selection(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    query: &QueryExpr,
) -> Result<(), String> {
    let qe = QualifiedEntityKey {
        entry_id: query
            .catalog_entry_id
            .as_deref()
            .unwrap_or(session.entry_id.as_str())
            .to_string(),
        entity: query.entity.to_string(),
    };
    let Some(cgs) = cgs_for_qualified_entity(session, &qe) else {
        return Ok(());
    };
    let Some(pred) = query.predicate.as_ref() else {
        return Ok(());
    };
    walk_predicate(session, state, node_id, pred, |field| {
        query_or_search_slot_value_ref(cgs.as_ref(), qe.entity.as_str(), field)
    })
}

fn query_or_search_slot_value_ref(
    cgs: &plasm_core::schema::CGS,
    entity: &str,
    field: &str,
) -> Option<String> {
    for kind in [CapabilityKind::Query, CapabilityKind::Search] {
        for cap in cgs.find_capabilities(entity, kind) {
            if let Some(key) = cap
                .query_surface_fields()
                .find(|f| f.name == field)
                .and_then(input_value_ref_key)
            {
                return Some(key.to_string());
            }
        }
    }
    None
}

fn validate_targeted(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    inv: &impl plasm_core::expr::TargetedCall,
    catalog_entry_id: Option<&str>,
) -> Result<(), String> {
    let Some(input) = inv.input() else {
        return Ok(());
    };
    validate_invocation_object(
        session,
        state,
        node_id,
        inv.capability().as_str(),
        inv.target().entity_type.as_str(),
        catalog_entry_id,
        &input.to_value(),
    )
}

fn validate_create(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    create: &CreateExpr,
) -> Result<(), String> {
    validate_invocation_object(
        session,
        state,
        node_id,
        create.capability.as_str(),
        create.entity.as_str(),
        create.catalog_entry_id.as_deref(),
        &create.input.to_value(),
    )
}

fn validate_invocation_object(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    capability: &str,
    entity: &str,
    catalog_entry_id: Option<&str>,
    value: &Value,
) -> Result<(), String> {
    let qe = QualifiedEntityKey {
        entry_id: catalog_entry_id
            .unwrap_or(session.entry_id.as_str())
            .to_string(),
        entity: entity.to_string(),
    };
    let Some(cgs) = cgs_for_qualified_entity(session, &qe) else {
        return Ok(());
    };
    let Some(cap) = cgs.get_capability(capability) else {
        return Ok(());
    };
    let Some(obj) = value.as_object() else {
        return Ok(());
    };
    let fields: Vec<_> = cap.invocation_object_fields().collect();
    for (param, val) in obj {
        let target = fields
            .iter()
            .find(|f| f.name == *param)
            .and_then(|f| input_value_ref_key(f))
            .map(str::to_string);
        reject_value_refs(session, state, node_id, param, val, target.as_deref())?;
    }
    Ok(())
}

fn walk_predicate(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    pred: &Predicate,
    target_for_field: impl Fn(&str) -> Option<String> + Copy,
) -> Result<(), String> {
    match pred {
        Predicate::Comparison { field, value, .. } => {
            let target = target_for_field(field);
            reject_value_refs(
                session,
                state,
                node_id,
                field,
                &value.to_value(),
                target.as_deref(),
            )
        }
        Predicate::And { args } | Predicate::Or { args } => {
            for p in args {
                walk_predicate(session, state, node_id, p, target_for_field)?;
            }
            Ok(())
        }
        Predicate::Not { predicate } => {
            walk_predicate(session, state, node_id, predicate, target_for_field)
        }
        Predicate::ExistsRelation { predicate, .. } => {
            if let Some(inner) = predicate {
                walk_predicate(session, state, node_id, inner, target_for_field)?;
            }
            Ok(())
        }
        Predicate::True | Predicate::False => Ok(()),
    }
}

fn reject_value_refs(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    param: &str,
    value: &Value,
    target_ref: Option<&str>,
) -> Result<(), String> {
    match value {
        Value::GetScalarExtract(extract) => {
            let qe = QualifiedEntityKey {
                entry_id: extract
                    .catalog_entry_id
                    .clone()
                    .unwrap_or_else(|| session.entry_id.clone()),
                entity: extract.entity.clone(),
            };
            let source_ref = field_value_ref_key(session, &qe, &extract.wire);
            let Some(source_ref) = source_ref else {
                return Ok(());
            };
            let target = target_ref.unwrap_or("");
            if !is_password_value_ref(&source_ref) && !is_password_value_ref(target) {
                return Ok(());
            }
            if source_ref == target {
                return Ok(());
            }
            Err(ra9_password_domain(node_id, param))
        }
        Value::PlasmInputRef(r) => reject_input_ref(session, state, node_id, param, r, target_ref),
        Value::Array(items) => {
            for item in items {
                reject_value_refs(session, state, node_id, param, item, target_ref)?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for v in fields.values() {
                reject_value_refs(session, state, node_id, param, v, target_ref)?;
            }
            Ok(())
        }
        Value::UnionCtor { ctor_fields, .. } => {
            for v in ctor_fields.values() {
                reject_value_refs(session, state, node_id, param, v, target_ref)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn reject_input_ref(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    param: &str,
    r: &PlasmInputRef,
    target_ref: Option<&str>,
) -> Result<(), String> {
    let source_ref = match r {
        PlasmInputRef::NodeInput { node, path } => {
            source_value_ref_key(session, state, node, path, 0)
        }
        PlasmInputRef::RowBinding { binding, path } => {
            source_value_ref_key(session, state, binding, path, 0)
        }
    };
    let Some(source_ref) = source_ref else {
        return Ok(());
    };
    let target = target_ref.unwrap_or("");
    if !is_password_value_ref(&source_ref) && !is_password_value_ref(target) {
        return Ok(());
    }
    if source_ref == target {
        return Ok(());
    }
    Err(ra9_password_domain(node_id, param))
}

fn source_value_ref_key(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node: &str,
    path: &[String],
    depth: u8,
) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let dag = state.get(node)?;
    match &dag.source {
        DagNodeSource::ScalarExtract { source, wire } => {
            if path.is_empty() {
                source_value_ref_key(
                    session,
                    state,
                    source,
                    std::slice::from_ref(wire),
                    depth + 1,
                )
            } else {
                source_value_ref_key(session, state, source, path, depth + 1)
            }
        }
        DagNodeSource::Derive { source, value, .. } => {
            if path.is_empty() {
                derive_source_value_ref_key(session, state, source, value, depth + 1)
            } else {
                source_value_ref_key(session, state, source, path, depth + 1)
            }
        }
        DagNodeSource::Compute { source, op, .. } => {
            let remapped = remap_compute_policy_path(op, path);
            source_value_ref_key(session, state, source, &remapped, depth + 1)
        }
        DagNodeSource::Surface {
            qualified_entity, ..
        }
        | DagNodeSource::RelationTraversal {
            qualified_entity, ..
        }
        | DagNodeSource::ForEach {
            qualified_entity, ..
        } => {
            let wire = path.last()?;
            field_value_ref_key(session, qualified_entity, wire)
        }
        DagNodeSource::Data(_) | DagNodeSource::IterateUntil { .. } => None,
    }
}

fn remap_compute_policy_path(op: &ComputeOp, path: &[String]) -> Vec<String> {
    if path.is_empty() {
        return path.to_vec();
    }
    match op {
        ComputeOp::With { columns } => {
            for col in columns.iter().rev() {
                if col.name.as_str() != path[0] {
                    continue;
                }
                let WithExpr::Field(fp) = &col.expr else {
                    return path.to_vec();
                };
                let mut out = fp.segments().to_vec();
                out.extend_from_slice(&path[1..]);
                return out;
            }
            path.to_vec()
        }
        ComputeOp::Project { fields } => {
            for (name, fp) in fields {
                if name.as_str() != path[0] {
                    continue;
                }
                let mut out = fp.segments().to_vec();
                out.extend_from_slice(&path[1..]);
                return out;
            }
            path.to_vec()
        }
        _ => path.to_vec(),
    }
}

fn derive_source_value_ref_key(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    source: &str,
    value: &PlanValue,
    depth: u8,
) -> Option<String> {
    let mut refs = Vec::new();
    collect_plan_value_field_paths(value, &mut refs);
    let mut resolved = Vec::new();
    for p in refs {
        if let Some(key) = source_value_ref_key(session, state, source, &p, depth) {
            if is_password_value_ref(&key) {
                return Some(key);
            }
            resolved.push(key);
        }
    }
    resolved.into_iter().next()
}

fn collect_plan_value_field_paths(value: &PlanValue, out: &mut Vec<Vec<String>>) {
    match value {
        PlanValue::BindingSymbol { path, .. } | PlanValue::NodeSymbol { path, .. } => {
            if !path.is_empty() {
                out.push(path.clone());
            }
        }
        PlanValue::Object { fields } => {
            for v in fields.values() {
                collect_plan_value_field_paths(v, out);
            }
        }
        PlanValue::Array { items } => {
            for v in items {
                collect_plan_value_field_paths(v, out);
            }
        }
        _ => {}
    }
}

fn field_value_ref_key(
    session: &ExecuteSession,
    qe: &QualifiedEntityKey,
    wire: &str,
) -> Option<String> {
    let cgs = cgs_for_qualified_entity(session, qe)?;
    let ent = cgs.get_entity(qe.entity.as_str())?;
    let field = ent.fields.get(wire)?;
    Some(field.value_domain_key().as_str().to_string())
}

fn input_value_ref_key(field: &plasm_core::schema::InputFieldSchema) -> Option<&str> {
    match &field.wire {
        InputFieldWire::Registry(k) => Some(k.as_str()),
        InputFieldWire::Inline(_) => None,
    }
}

fn is_password_value_ref(key: &str) -> bool {
    key == PASSWORD_EXACT || key.ends_with(PASSWORD_SUFFIX)
}

fn ra9_password_domain(node_id: &str, param: &str) -> String {
    format!(
        "RA-9: Plasm program `{node_id}`: password-domain value_ref cannot fill `{param}` — source and target keys must match"
    )
}
