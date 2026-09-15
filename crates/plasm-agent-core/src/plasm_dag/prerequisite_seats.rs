//! RA-17: prerequisite seats may only be filled from their deployed provider.

use super::prelude::*;
use super::types::{CompileState, DagNodeSource};
use plasm_core::prerequisites::{
    validate_deployed_prerequisite_seats, CapabilityRef, InputLane, InputPath, SeatWiring,
};
use plasm_core::{CreateExpr, PlasmInputRef, Predicate, QueryExpr, Value};

/// Reject a foreign-provider acquisition (or shared binding) on a deployed seat.
pub(in crate::plasm_dag) fn validate_prerequisite_seat_bind(
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
            validate_prerequisite_seat_bind(session, state, node_id, &chain.source)?;
            if let plasm_core::ChainStep::Explicit { expr } = &chain.step {
                validate_prerequisite_seat_bind(session, state, node_id, expr)?;
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
    let catalog = query
        .catalog_entry_id
        .as_deref()
        .unwrap_or(session.entry_id.as_str());
    let Some(capability) = query.capability_name.as_deref() else {
        return Ok(());
    };
    let Some(pred) = query.predicate.as_ref() else {
        return Ok(());
    };
    let mut wirings = Vec::new();
    collect_predicate_wirings(session, state, catalog, capability, pred, &mut wirings);
    check_seats(session, node_id, catalog, capability, &wirings)
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
        create.catalog_entry_id.as_deref(),
        &create.input.to_value(),
    )
}

fn validate_invocation_object(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node_id: &str,
    capability: &str,
    catalog_entry_id: Option<&str>,
    value: &Value,
) -> Result<(), String> {
    let catalog = catalog_entry_id.unwrap_or(session.entry_id.as_str());
    let Some(obj) = value.as_object() else {
        return Ok(());
    };
    let mut wirings = Vec::new();
    for (param, val) in obj {
        collect_value_wirings(
            session,
            state,
            catalog,
            capability,
            param,
            val,
            &mut wirings,
        );
    }
    check_seats(session, node_id, catalog, capability, &wirings)
}

fn check_seats(
    session: &ExecuteSession,
    node_id: &str,
    catalog: &str,
    capability: &str,
    wirings: &[SeatWiring],
) -> Result<(), String> {
    if wirings.is_empty() || session.prerequisite_deployments.bindings.is_empty() {
        return Ok(());
    }
    let catalogs: std::collections::BTreeMap<String, &plasm_core::schema::CGS> = session
        .contexts_by_entry
        .iter()
        .map(|(id, ctx)| (id.clone(), ctx.cgs.as_ref()))
        .collect();
    validate_deployed_prerequisite_seats(
        &catalogs,
        &session.prerequisite_deployments,
        &CapabilityRef {
            catalog: catalog.to_string(),
            capability: capability.to_string(),
        },
        wirings,
    )
    .map_err(|msg| {
        format!(
            "RA-17: Plasm program `{node_id}`: {}",
            strip_ra17_prefix(&msg)
        )
    })
}

fn strip_ra17_prefix(msg: &str) -> &str {
    msg.strip_prefix("RA-17: ").unwrap_or(msg)
}

fn collect_predicate_wirings(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    catalog: &str,
    capability: &str,
    pred: &Predicate,
    out: &mut Vec<SeatWiring>,
) {
    match pred {
        Predicate::Comparison { field, value, .. } => {
            collect_value_wirings(
                session,
                state,
                catalog,
                capability,
                field,
                &value.to_value(),
                out,
            );
        }
        Predicate::And { args } | Predicate::Or { args } => {
            for p in args {
                collect_predicate_wirings(session, state, catalog, capability, p, out);
            }
        }
        Predicate::Not { predicate } => {
            collect_predicate_wirings(session, state, catalog, capability, predicate, out)
        }
        Predicate::ExistsRelation { predicate, .. } => {
            if let Some(inner) = predicate {
                collect_predicate_wirings(session, state, catalog, capability, inner, out);
            }
        }
        Predicate::True | Predicate::False => {}
    }
}

fn collect_value_wirings(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    catalog: &str,
    capability: &str,
    param: &str,
    value: &Value,
    out: &mut Vec<SeatWiring>,
) {
    match value {
        Value::PlasmInputRef(r) => {
            out.push(SeatWiring {
                input: seat_input_path(session, catalog, capability, param),
                binding: binding_name(r),
                source_catalog: source_catalog_for_ref(state, r),
            });
        }
        Value::GetScalarExtract(extract) => {
            out.push(SeatWiring {
                input: seat_input_path(session, catalog, capability, param),
                binding: None,
                source_catalog: Some(
                    extract
                        .catalog_entry_id
                        .clone()
                        .unwrap_or_else(|| session.entry_id.clone()),
                ),
            });
        }
        Value::Array(items) => {
            for item in items {
                collect_value_wirings(session, state, catalog, capability, param, item, out);
            }
        }
        Value::Object(fields) => {
            for (k, v) in fields {
                collect_value_wirings(session, state, catalog, capability, k, v, out);
            }
        }
        Value::UnionCtor { ctor_fields, .. } => {
            for (k, v) in ctor_fields {
                collect_value_wirings(session, state, catalog, capability, k, v, out);
            }
        }
        _ => {}
    }
}

fn seat_input_path(
    session: &ExecuteSession,
    catalog: &str,
    capability: &str,
    param: &str,
) -> InputPath {
    if let Some(cgs) = session
        .contexts_by_entry
        .get(catalog)
        .map(|c| c.cgs.as_ref())
        .or_else(|| (session.entry_id == catalog).then(|| session.cgs.as_ref()))
    {
        if let Some(reqs) = cgs.prerequisites.requirements.get(capability) {
            for req in reqs {
                for binding in &req.bindings {
                    if binding.input.path.last().is_some_and(|p| p == param) {
                        return binding.input.clone();
                    }
                }
            }
        }
    }
    InputPath {
        lane: InputLane::Payload,
        path: vec![param.to_string()],
    }
}

fn binding_name(r: &PlasmInputRef) -> Option<String> {
    match r {
        PlasmInputRef::NodeInput { node, .. } => Some(node.clone()),
        PlasmInputRef::RowBinding { binding, .. } => Some(binding.clone()),
    }
}

fn source_catalog_for_ref(state: &CompileState<'_>, r: &PlasmInputRef) -> Option<String> {
    match r {
        PlasmInputRef::NodeInput { node, .. } => source_catalog(state, node, 0),
        PlasmInputRef::RowBinding { binding, .. } => source_catalog(state, binding, 0),
    }
}

fn source_catalog(state: &CompileState<'_>, node: &str, depth: u8) -> Option<String> {
    if depth > 8 {
        return None;
    }
    let dag = state.get(node)?;
    match &dag.source {
        DagNodeSource::ScalarExtract { source, .. } => source_catalog(state, source, depth + 1),
        DagNodeSource::Derive { source, .. } | DagNodeSource::Compute { source, .. } => {
            source_catalog(state, source, depth + 1)
        }
        DagNodeSource::Surface {
            qualified_entity, ..
        }
        | DagNodeSource::RelationTraversal {
            qualified_entity, ..
        } => Some(qualified_entity.entry_id.clone()),
        DagNodeSource::Data(_)
        | DagNodeSource::ForEach { .. }
        | DagNodeSource::IterateUntil { .. } => None,
    }
}
