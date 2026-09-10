//! Client-side predicate matching and ref extraction.

use super::*;

pub(crate) fn client_side_predicate_matches(
    entity: &CachedEntity,
    predicate: &plasm_core::Predicate,
) -> Result<bool, RuntimeError> {
    use plasm_core::CompOp;
    match predicate {
        plasm_core::Predicate::True => Ok(true),
        plasm_core::Predicate::False => Ok(false),
        plasm_core::Predicate::Comparison { field, op, value } => {
            let rhs = value.to_value();
            let Some(actual_tf) = entity.get_field(field) else {
                return Ok(*op == CompOp::Exists && matches!(rhs, Value::Null));
            };
            let actual = actual_tf.to_value();
            let money_err = |e: plasm_core::CrossCurrencyError| {
                RuntimeError::from(plasm_core::TypeError::from(e))
            };
            Ok(match op {
                CompOp::Eq => plasm_core::money::values_eq(&actual, &rhs).map_err(money_err)?,
                CompOp::Neq => !plasm_core::money::values_eq(&actual, &rhs).map_err(money_err)?,
                CompOp::Gt => plasm_core::money::values_ord(&actual, &rhs)
                    .map_err(money_err)?
                    .is_some_and(|o| o.is_gt()),
                CompOp::Lt => plasm_core::money::values_ord(&actual, &rhs)
                    .map_err(money_err)?
                    .is_some_and(|o| o.is_lt()),
                CompOp::Gte => plasm_core::money::values_ord(&actual, &rhs)
                    .map_err(money_err)?
                    .is_some_and(|o| o.is_ge()),
                CompOp::Lte => plasm_core::money::values_ord(&actual, &rhs)
                    .map_err(money_err)?
                    .is_some_and(|o| o.is_le()),
                CompOp::Contains => actual.contains(&rhs),
                CompOp::In => match &rhs {
                    Value::Array(arr) => arr.contains(&actual),
                    _ => false,
                },
                CompOp::Exists => !matches!(actual, Value::Null),
            })
        }
        plasm_core::Predicate::And { args } => {
            for a in args {
                if !client_side_predicate_matches(entity, a)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        plasm_core::Predicate::Or { args } => {
            for a in args {
                if client_side_predicate_matches(entity, a)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        plasm_core::Predicate::Not { predicate: inner } => {
            Ok(!client_side_predicate_matches(entity, inner)?)
        }
        plasm_core::Predicate::ExistsRelation { .. } => Ok(true),
    }
}

pub(crate) fn filter_entities_by_predicate(
    entities: Vec<CachedEntity>,
    pred: &plasm_core::Predicate,
) -> Result<Vec<CachedEntity>, RuntimeError> {
    let mut out = Vec::with_capacity(entities.len());
    for e in entities {
        if client_side_predicate_matches(&e, pred)? {
            out.push(e);
        }
    }
    Ok(out)
}

/// Strip comparisons against non-entity fields from a predicate, returning the
/// entity-field-only portion suitable for client-side filtering.
///
/// Comparisons against fields **not** present in the entity schema are dropped —
/// they represent capability parameters (scope, filter, search, sort) that were
/// already handled server-side by the CML request template. Keeping them would
/// incorrectly eliminate all decoded entities (e.g. `block_id` in a
/// `block_children_query` result).
///
/// Comparisons against fields that are **also** declared as capability `parameters`
/// are dropped when those names appear in `cap_params`: the request already
/// carried them, and the response may round or normalize values (e.g. Open-Meteo
/// `latitude` / `longitude`).
///
/// Returns `None` when the entire predicate reduces to an unconditional pass
/// (i.e. nothing remains to filter client-side).
pub(crate) fn entity_field_predicate(
    pred: &plasm_core::Predicate,
    entity: &plasm_core::EntityDef,
    cap_params: Option<&HashSet<String>>,
) -> Option<plasm_core::Predicate> {
    use plasm_core::Predicate;
    match pred {
        Predicate::True | Predicate::False => Some(pred.clone()),
        Predicate::Comparison { field, .. } => {
            if !entity.fields.contains_key(field.as_str()) {
                return None;
            }
            if let Some(names) = cap_params {
                if names.contains(field) {
                    return None;
                }
            }
            Some(pred.clone())
        }
        Predicate::And { args } => {
            let kept: Vec<_> = args
                .iter()
                .filter_map(|a| entity_field_predicate(a, entity, cap_params))
                .collect();
            match kept.len() {
                0 => None,
                1 => Some(kept.into_iter().next().unwrap()),
                _ => Some(Predicate::And { args: kept }),
            }
        }
        Predicate::Or { args } => {
            let kept: Vec<_> = args
                .iter()
                .filter_map(|a| entity_field_predicate(a, entity, cap_params))
                .collect();
            match kept.len() {
                0 => None,
                1 => Some(kept.into_iter().next().unwrap()),
                _ => Some(Predicate::Or { args: kept }),
            }
        }
        Predicate::Not { predicate: inner } => entity_field_predicate(inner, entity, cap_params)
            .map(|p| Predicate::Not {
                predicate: Box::new(p),
            }),
        // Relation predicates are never entity scalar fields; leave them for cross-entity logic.
        Predicate::ExistsRelation { .. } => Some(pred.clone()),
    }
}

pub(crate) fn capability_param_names(capability: &plasm_core::CapabilitySchema) -> HashSet<String> {
    capability.input_fields().map(|f| f.name.clone()).collect()
}

/// Extract an EntityRef field or declared-relation target ID from a cached entity.
pub(crate) fn extract_ref_id(entity: &CachedEntity, selector: &str, cgs: &CGS) -> Option<String> {
    if let Some(source_ent) = cgs.get_entity(entity.reference.entity_type.as_str()) {
        if let Some(rel) = source_ent.relations.get(selector) {
            if let Some(target_ent) = cgs.get_entity(rel.target_resource.as_str()) {
                let key_vars = source_ent
                    .key_vars
                    .iter()
                    .map(|k| k.as_str().to_string())
                    .collect::<Vec<_>>();
                let mut identity = plasm_core::row_identity_from_parts(
                    plasm_core::QualifiedEntityKey::new(
                        String::new(),
                        entity.reference.entity_type.to_string(),
                    ),
                    entity.reference.clone(),
                    &entity.relations,
                    source_ent.id_field.as_str(),
                    &key_vars,
                );
                for rel_name in source_ent.relations.keys() {
                    if identity.ambient.contains_key(rel_name.as_str()) {
                        continue;
                    }
                    if let Some(tf) = entity.get_field(rel_name.as_str()) {
                        if let Value::String(s) = tf.to_value() {
                            if !s.is_empty() {
                                identity.ambient.insert(rel_name.as_str().to_string(), s);
                            }
                        }
                    }
                }
                if let Ok(target_ref) =
                    plasm_core::resolve_relation_target_id(&identity, selector, target_ent)
                {
                    let slot = target_ref.primary_slot_str();
                    if !slot.is_empty() {
                        return Some(slot);
                    }
                }
            }
        }
    }
    let v = entity.get_field(selector).map(|tf| tf.to_value())?;
    match v {
        Value::String(s) if !s.is_empty() => Some(s),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        _ => None,
    }
}

pub(crate) fn extract_predicate_vars(predicate: &plasm_core::Predicate, env: &mut CmlEnv) {
    // First collect all field→value pairs, accumulating multi-value (In/Contains) arrays.
    let mut accumulator: indexmap::IndexMap<String, Vec<Value>> = indexmap::IndexMap::new();
    collect_predicate_vars(predicate, &mut accumulator);

    for (field, mut values) in accumulator {
        match values.len() {
            0 => {}
            1 => {
                env.insert(field, values.remove(0));
            }
            _ => {
                env.insert(field, Value::Array(values));
            }
        }
    }
}

pub(crate) fn collect_predicate_vars(
    predicate: &plasm_core::Predicate,
    acc: &mut indexmap::IndexMap<String, Vec<Value>>,
) {
    match predicate {
        plasm_core::Predicate::Comparison { field, op, value } => {
            let rhs = value.to_value();
            // Unfilled plan holes must not pollute CML env (would mask missing Bearer tokens as
            // debug strings). Skip — compile then fails VariableNotFound if required.
            if matches!(rhs, Value::PlasmInputRef(_)) {
                return;
            }
            match op {
                // In/Contains: accumulate into an array for the field
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
                // All other ops: single scalar value — last one wins per field
                _ => {
                    acc.entry(field.clone()).or_default().clear();
                    acc.entry(field.clone()).or_default().push(rhs);
                }
            }
        }
        plasm_core::Predicate::And { args } => {
            for arg in args {
                collect_predicate_vars(arg, acc);
            }
        }
        plasm_core::Predicate::Or { args } => {
            for arg in args {
                collect_predicate_vars(arg, acc);
            }
        }
        _ => {}
    }
}
