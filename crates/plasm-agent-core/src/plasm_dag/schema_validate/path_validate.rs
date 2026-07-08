//! Compute field-path validation against row contracts.

use super::super::prelude::*;
use super::super::types::{CompileState, DagNode, DagNodeSource};
use super::catalog::{
    agent_program_error, capability_for_surface_expr, cgs_for_qualified_entity,
    is_opaque_passthrough_compute_schema, logical_row_field_paths_for_entity,
    row_contract_field_error, single_segment_teaching_field_hint,
};
use super::dag_lookup::{
    logical_row_field_paths_for_surface_node, resolve_immediate_compute_schema,
    resolve_qualified_entity_for_dag_source, resolve_surface_dag_node,
};

pub(in crate::plasm_dag) fn validate_compute_paths_for_schema(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    schema: &SyntheticResultSchema,
    paths: &[FieldPath],
    _op_label: &str,
) -> Result<(), String> {
    let allowed: std::collections::BTreeSet<String> = schema
        .fields
        .iter()
        .map(|f| f.name.as_str().to_string())
        .collect();
    for path in paths {
        let segs = path.segments();
        if segs.len() == 1 {
            let raw = segs[0].as_str();
            if allowed.contains(raw) {
                continue;
            }
        }
        let wire = if segs.len() == 1 {
            crate::plasm_plan_run::resolve_wire_field_token(
                session,
                symbol_map_cross_cache,
                qe,
                segs[0].as_str(),
            )?
        } else {
            path.dotted()
        };
        if allowed.contains(&wire) {
            continue;
        }
        return Err(agent_program_error(
            format!("`{wire}` is not a row field on this binding's compute output."),
            Some("Use wire field names from the teaching TSV (e.g. `.sort(height)`, `[title,…]`)."),
        ));
    }
    Ok(())
}
pub(in crate::plasm_dag) fn validate_compute_paths_for_dag_source(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    staged: &[DagNode],
    source_id: &str,
    paths: &[FieldPath],
    op_label: &str,
) -> Result<(), String> {
    if let Some(schema) = resolve_immediate_compute_schema(state, staged, source_id) {
        if !is_opaque_passthrough_compute_schema(&schema) {
            let qe = resolve_qualified_entity_for_dag_source(state, staged, source_id.to_string());
            return validate_compute_paths_for_schema(
                session,
                state.cross_cache,
                qe.as_ref(),
                &schema,
                paths,
                op_label,
            );
        }
    }
    if let Some(surface) = resolve_surface_dag_node(state, staged, source_id.to_string()) {
        if let Some(allowed) = logical_row_field_paths_for_surface_node(session, surface)? {
            return validate_compute_paths_for_allowed_set(
                session,
                state.cross_cache,
                surface,
                &allowed,
                paths,
                op_label,
            );
        }
    }
    if let Some(qe) = resolve_qualified_entity_for_dag_source(state, staged, source_id.to_string())
    {
        return validate_compute_paths_for_entity(session, state.cross_cache, &qe, paths, op_label);
    }
    Ok(())
}
pub(in crate::plasm_dag) fn validate_surface_inline_projection(
    session: &ExecuteSession,
    state: &CompileState<'_>,
    node: &DagNode,
) -> Result<(), String> {
    let parsed = match &node.source {
        DagNodeSource::Surface { parsed, .. } | DagNodeSource::RelationTraversal { parsed, .. } => {
            parsed
        }
        _ => return Ok(()),
    };
    let Some(fields) = parsed.projection.as_ref() else {
        return Ok(());
    };
    if fields.is_empty() {
        return Ok(());
    }
    let paths: Vec<FieldPath> = fields
        .iter()
        .map(|f| FieldPath::from_dotted(f.as_str()))
        .collect::<Result<_, _>>()?;
    if let Some(allowed) = logical_row_field_paths_for_surface_node(session, node)? {
        validate_compute_paths_for_allowed_set(
            session,
            state.cross_cache,
            node,
            &allowed,
            &paths,
            "surface projection",
        )?;
    }
    Ok(())
}

pub(in crate::plasm_dag) fn validate_compute_paths_for_allowed_set(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    surface_node: &DagNode,
    allowed: &BTreeSet<Vec<String>>,
    paths: &[FieldPath],
    op_label: &str,
) -> Result<(), String> {
    let qe = match &surface_node.source {
        DagNodeSource::Surface {
            qualified_entity, ..
        }
        | DagNodeSource::RelationTraversal {
            qualified_entity, ..
        } => qualified_entity,
        _ => {
            return Err(
                "Plasm program internal: validate_compute_paths_for_allowed_set requires surface"
                    .into(),
            );
        }
    };
    let cgs = cgs_for_qualified_entity(session, qe).ok_or_else(|| {
        format!(
            "catalog `{}` is not loaded for entity `{}`",
            qe.entry_id, qe.entity
        )
    })?;
    let cap = match &surface_node.source {
        DagNodeSource::Surface { parsed, .. } | DagNodeSource::RelationTraversal { parsed, .. } => {
            capability_for_surface_expr(cgs.as_ref(), &parsed.expr)?
        }
        _ => None,
    };
    for path in paths {
        let mut segs: Vec<String> = path.segments().to_vec();
        if segs.len() == 1 {
            let wire = crate::plasm_plan_run::resolve_wire_field_token(
                session,
                symbol_map_cross_cache,
                Some(qe),
                segs[0].as_str(),
            )?;
            segs[0] = wire.clone();
            if allowed.contains(&segs) {
                continue;
            }
        } else if allowed.contains(&segs) {
            continue;
        }
        let cols: Vec<String> = allowed.iter().filter_map(|s| s.first().cloned()).collect();
        let wire = path.dotted();
        let wire_for_input = if segs.len() == 1 {
            segs[0].as_str()
        } else {
            wire.as_str()
        };
        return Err(row_contract_field_error(
            session,
            symbol_map_cross_cache,
            qe,
            cap,
            path,
            wire_for_input,
            &cols,
            op_label,
        ));
    }
    Ok(())
}

pub(in crate::plasm_dag) fn validate_compute_paths_for_entity(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: &QualifiedEntityKey,
    paths: &[FieldPath],
    op_label: &str,
) -> Result<(), String> {
    let cgs = cgs_for_qualified_entity(session, qe).ok_or_else(|| {
        format!(
            "Plasm program internal: catalog `{}` is not loaded for entity `{}`",
            qe.entry_id, qe.entity
        )
    })?;
    let ent = cgs.get_entity(qe.entity.as_str()).ok_or_else(|| {
        format!(
            "Plasm program internal: unknown entity `{}` in catalog `{}`",
            qe.entity, qe.entry_id
        )
    })?;
    let allowed = logical_row_field_paths_for_entity(ent);
    for path in paths {
        let mut segs: Vec<String> = path.segments().to_vec();
        if segs.len() == 1 {
            let wire = crate::plasm_plan_run::resolve_wire_field_token(
                session,
                symbol_map_cross_cache,
                Some(qe),
                segs[0].as_str(),
            )?;
            segs[0] = wire;
        }
        if allowed.contains(&segs) {
            continue;
        }
        let hint = single_segment_teaching_field_hint(session, symbol_map_cross_cache, qe, path);
        return Err(format!(
            "Plasm program {op_label}: field path `{}` is not a row field of entity `{}` (catalog entry `{}`). Use wire field names (and taught `r#` for relations) from the active TSV for this entity — mixing another entity's symbols yields null columns.{hint}",
            path.dotted(),
            qe.entity,
            qe.entry_id
        ));
    }
    Ok(())
}
