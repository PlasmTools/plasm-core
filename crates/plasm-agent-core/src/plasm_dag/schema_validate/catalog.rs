//! CGS catalog helpers and diagnostic formatting.

use super::super::prelude::*;

pub(in crate::plasm_dag) fn cgs_for_qualified_entity(
    session: &ExecuteSession,
    qe: &QualifiedEntityKey,
) -> Option<Arc<plasm_core::schema::CGS>> {
    session
        .contexts_by_entry
        .get(qe.entry_id.as_str())
        .map(|c| c.cgs.clone())
        .or_else(|| (session.entry_id == qe.entry_id).then(|| session.cgs.clone()))
}

/// Logical row keys materialized by entity decode (`FieldDecoder` stores each field under its CGS name).
pub(in crate::plasm_dag) fn logical_row_field_paths_for_entity(
    ent: &EntityDef,
) -> BTreeSet<Vec<String>> {
    let mut set = BTreeSet::new();
    for name in ent.fields.keys() {
        set.insert(vec![name.as_str().to_string()]);
    }
    for rel_name in ent.relations.keys() {
        set.insert(vec![rel_name.as_str().to_string()]);
    }
    set
}

pub(in crate::plasm_dag) fn logical_row_field_paths_from_names(
    names: &[String],
) -> BTreeSet<Vec<String>> {
    names.iter().map(|n| vec![n.clone()]).collect()
}
pub(in crate::plasm_dag) fn capability_for_surface_expr<'a>(
    cgs: &'a plasm_core::schema::CGS,
    expr: &'a Expr,
) -> Result<Option<&'a CapabilitySchema>, String> {
    match expr {
        Expr::Query(q) => {
            let cap = if let Some(name) = q.capability_name.as_deref() {
                cgs.get_capability(name).ok_or_else(|| {
                    format!(
                        "unknown query capability `{name}` for entity `{}`",
                        q.entity
                    )
                })?
            } else {
                query_resolve::resolve_query_capability(q, cgs).map_err(|e| e.to_string())?
            };
            Ok(Some(cap))
        }
        Expr::Get(g) => Ok(cgs
            .find_capabilities(g.reference.entity_type.as_str(), CapabilityKind::Get)
            .into_iter()
            .next()),
        Expr::Create(c) => Ok(cgs.get_capability(c.capability.as_str())),
        Expr::Delete(d) => Ok(cgs.get_capability(d.capability.as_str())),
        Expr::Invoke(i) => Ok(cgs.get_capability(i.capability.as_str())),
        Expr::Chain(_)
        | Expr::TeachingValue { .. }
        | Expr::Page(_)
        | Expr::Wait(_)
        | Expr::Cancel(_) => Ok(None),
    }
}
pub(in crate::plasm_dag) fn infer_entity_row_columns(
    session: &ExecuteSession,
    qe: &QualifiedEntityKey,
) -> Result<Vec<OutputName>, String> {
    let cgs = cgs_for_qualified_entity(session, qe).ok_or_else(|| {
        format!(
            "catalog `{}` is not loaded for entity `{}`",
            qe.entry_id, qe.entity
        )
    })?;
    let ent = cgs.get_entity(qe.entity.as_str()).ok_or_else(|| {
        format!(
            "unknown entity `{}` in catalog `{}`",
            qe.entity, qe.entry_id
        )
    })?;
    let paths = logical_row_field_paths_for_entity(ent);
    paths
        .into_iter()
        .map(|segs| OutputName::new(segs.join(".")))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}
pub(in crate::plasm_dag) fn single_segment_teaching_field_hint(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: &QualifiedEntityKey,
    path: &FieldPath,
) -> String {
    let segs = path.segments().to_vec();
    if segs.len() != 1 {
        return String::new();
    }
    let wire = segs[0].as_str();
    let map = symbol_map_for_plasm_surface_parse(session, symbol_map_cross_cache);
    let sym = map.ident_sym_entity_field_for(qe.entry_id.as_str(), qe.entity.as_str(), wire);
    if sym != wire {
        format!(" For `{wire}` the active teaching-table symbol is `{sym}`.")
    } else {
        String::new()
    }
}

pub(in crate::plasm_dag) fn is_opaque_passthrough_compute_schema(
    schema: &SyntheticResultSchema,
) -> bool {
    schema.fields.len() == 1
        && schema.fields[0].name.as_str() == "value"
        && matches!(schema.fields[0].value_kind, SyntheticValueKind::Unknown)
}

pub(in crate::plasm_dag) fn agent_program_error(
    head: impl AsRef<str>,
    help: Option<impl AsRef<str>>,
) -> String {
    if let Some(h) = help {
        format!("{}\nhelp: {}", head.as_ref(), h.as_ref())
    } else {
        head.as_ref().to_string()
    }
}
pub(in crate::plasm_dag) fn capability_input_param_wires(
    cap: &CapabilitySchema,
) -> BTreeSet<String> {
    let Some(is) = &cap.input_schema else {
        return BTreeSet::new();
    };
    let InputType::Object { fields, .. } = &is.input_type else {
        return BTreeSet::new();
    };
    fields.iter().map(|f| f.name.clone()).collect()
}

#[allow(clippy::too_many_arguments)]
pub(in crate::plasm_dag) fn row_contract_field_error(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    _qe: &QualifiedEntityKey,
    cap: Option<&CapabilitySchema>,
    _path: &FieldPath,
    wire: &str,
    _allowed_cols: &[String],
    _op_label: &str,
) -> String {
    let _ = (session, symbol_map_cross_cache);
    if let Some(cap) = cap {
        let inputs = capability_input_param_wires(cap);
        if inputs.contains(wire) {
            return agent_program_error(
                format!("`{wire}` is a query/capability input on this fetch, not a row field."),
                Some("Use `p#` from teaching `rows:` for row postfix (`.filter`, `[p#,…]`)."),
            );
        }
    }
    agent_program_error(
        format!("`{wire}` is not a row field on this binding's rows."),
        Some("Use `p#` symbols from the teaching `rows:` column for this binding."),
    )
}
pub(in crate::plasm_dag) fn resolve_compute_field_path(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    path: &FieldPath,
) -> Result<FieldPath, String> {
    let segs = path.segments();
    if segs.len() != 1 {
        return Ok(path.clone());
    }
    let wire = crate::plasm_plan_run::resolve_wire_field_token(
        session,
        symbol_map_cross_cache,
        qe,
        segs[0].as_str(),
    )?;
    FieldPath::from_dotted(&wire)
}

/// Sort keys may name synthetic compute outputs (e.g. `group_by` aggregate `n`) without wire resolution.
pub(in crate::plasm_dag) fn resolve_sort_field_path(
    session: &ExecuteSession,
    symbol_map_cross_cache: Option<&SymbolMapCrossRequestCache>,
    qe: Option<&QualifiedEntityKey>,
    source_schema: Option<&SyntheticResultSchema>,
    path: &FieldPath,
) -> Result<FieldPath, String> {
    let segs = path.segments();
    if segs.len() == 1 {
        let raw = segs[0].as_str();
        if let Some(schema) = source_schema {
            if schema.fields.iter().any(|f| f.name.as_str() == raw) {
                return FieldPath::from_dotted(raw);
            }
        }
    }
    resolve_compute_field_path(session, symbol_map_cross_cache, qe, path)
}
