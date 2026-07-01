use crate::predicate::Predicate;
use crate::symbol_tuning::SymbolMap;
use crate::CompOp;

use super::values::render_surface_value;

pub(crate) fn comp_op_str(op: CompOp) -> &'static str {
    match op {
        CompOp::Eq => "=",
        CompOp::Neq => "!=",
        CompOp::Gt => ">",
        CompOp::Lt => "<",
        CompOp::Gte => ">=",
        CompOp::Lte => "<=",
        CompOp::In => " in ",
        CompOp::Contains => "~",
        CompOp::Exists => " exists ",
    }
}

pub(crate) fn render_predicate_wire(
    pred: &Predicate,
    entity: &str,
    entry_id: Option<&str>,
    capability: Option<&str>,
    map: Option<&SymbolMap>,
) -> String {
    match pred {
        Predicate::True => "true".to_string(),
        Predicate::False => "false".to_string(),
        Predicate::Comparison { field, op, value } => {
            let f = field_token_wire(entry_id, entity, field, capability, map);
            let v = render_surface_value(&value.to_value());
            if v.is_empty() {
                f
            } else {
                format!("{f}{op}{v}", op = comp_op_str(*op))
            }
        }
        Predicate::And { args } => args
            .iter()
            .map(|p| render_predicate_wire(p, entity, entry_id, capability, map))
            .collect::<Vec<_>>()
            .join(","),
        Predicate::Or { args } => {
            let inner = args
                .iter()
                .map(|p| render_predicate_wire(p, entity, entry_id, capability, map))
                .collect::<Vec<_>>()
                .join(" OR ");
            format!("({inner})")
        }
        Predicate::Not { predicate } => {
            format!(
                "NOT ({})",
                render_predicate_wire(predicate, entity, entry_id, capability, map)
            )
        }
        Predicate::ExistsRelation {
            relation,
            predicate,
        } => {
            let rel = relation_token_wire(entry_id, entity, relation, map);
            match predicate {
                Some(p) => format!(
                    "EXISTS {rel} WHERE {}",
                    render_predicate_wire(p, entity, entry_id, capability, map)
                ),
                None => format!("EXISTS {rel}"),
            }
        }
    }
}

pub(crate) fn split_search_predicate(
    pred: &Predicate,
    q_field: &str,
    text: &mut String,
    filters: &mut Vec<Predicate>,
) {
    match pred {
        Predicate::And { args } => {
            for p in args {
                if let Predicate::Comparison { field, op, value } = p {
                    if field == q_field && *op == CompOp::Eq {
                        *text = render_surface_value(&value.to_value());
                        continue;
                    }
                }
                filters.push(p.clone());
            }
        }
        Predicate::Comparison { field, op, value } if field == q_field && *op == CompOp::Eq => {
            *text = render_surface_value(&value.to_value());
        }
        other => filters.push(other.clone()),
    }
}

fn field_token_wire(
    entry_id: Option<&str>,
    entity: &str,
    field: &str,
    capability: Option<&str>,
    map: Option<&SymbolMap>,
) -> String {
    if let Some(m) = map {
        if capability.is_some() {
            if let (Some(eid), Some(cap)) = (entry_id, capability) {
                return m.ident_sym_cap_param_for(eid, entity, cap, field);
            }
            return m.ident_sym_cap_param_for("", entity, capability.unwrap_or(""), field);
        }
        if let Some(eid) = entry_id {
            return m.ident_sym_entity_field_for(eid, entity, field);
        }
        return m.ident_sym_entity_field_for("", entity, field);
    }
    field.to_string()
}

fn relation_token_wire(
    entry_id: Option<&str>,
    entity: &str,
    relation: &str,
    map: Option<&SymbolMap>,
) -> String {
    if let Some(m) = map {
        if let Some(eid) = entry_id {
            return m.ident_sym_relation_for(eid, entity, relation);
        }
        return m.ident_sym_relation_for("", entity, relation);
    }
    relation.to_string()
}
