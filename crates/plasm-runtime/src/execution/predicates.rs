//! Client-side predicate matching and ref extraction.

use super::*;

pub(crate) enum PreparedEntityPredicate {
    True,
    False,
    Atom(Box<crate::row_predicate::PreparedRowPredicate>),
    And(Vec<Self>),
    Or(Vec<Self>),
    Not(Box<Self>),
}

impl PreparedEntityPredicate {
    pub(crate) fn new(
        cgs: &CGS,
        entity: &str,
        predicate: &plasm_core::Predicate,
    ) -> Result<Self, RuntimeError> {
        use plasm_core::{CompOp, PlanPredicateOp as Op, Predicate};
        Ok(match predicate {
            Predicate::True | Predicate::ExistsRelation { .. } => Self::True,
            Predicate::False => Self::False,
            Predicate::Comparison { field, op, value } => {
                let bound = crate::row_predicate::BoundRowPredicate {
                    field_path: plasm_core::FieldPath::from_dotted(field)
                        .map_err(crate::row_predicate::RowPredicateError::from)?,
                    op: match op {
                        CompOp::Eq => Op::Eq,
                        CompOp::Neq => Op::Ne,
                        CompOp::Gt => Op::Gt,
                        CompOp::Lt => Op::Lt,
                        CompOp::Gte => Op::Gte,
                        CompOp::Lte => Op::Lte,
                        CompOp::Contains => Op::Contains,
                        CompOp::In => Op::In,
                        CompOp::Exists => Op::Exists,
                    },
                    value: plasm_core::operand_binding::ResolvedValue::new(value.to_value())?,
                };
                Self::Atom(Box::new(
                    crate::row_predicate::PreparedRowPredicate::prepare(cgs, entity, &bound)?,
                ))
            }
            Predicate::And { args } => Self::And(
                args.iter()
                    .map(|p| Self::new(cgs, entity, p))
                    .collect::<Result<_, _>>()?,
            ),
            Predicate::Or { args } => Self::Or(
                args.iter()
                    .map(|p| Self::new(cgs, entity, p))
                    .collect::<Result<_, _>>()?,
            ),
            Predicate::Not { predicate } => Self::Not(Box::new(Self::new(cgs, entity, predicate)?)),
        })
    }

    fn evaluate(&self, entity: &CachedEntity) -> Result<Option<bool>, RuntimeError> {
        Ok(match self {
            Self::True => Some(true),
            Self::False => Some(false),
            Self::Atom(p) => p.evaluate_entity(entity)?,
            Self::Not(p) => p.evaluate(entity)?.map(|v| !v),
            Self::And(args) => {
                let mut result = Some(true);
                for p in args {
                    if result == Some(false) {
                        break;
                    }
                    result = match (result, p.evaluate(entity)?) {
                        (_, Some(false)) => Some(false),
                        (None, _) | (_, None) => None,
                        _ => Some(true),
                    };
                }
                result
            }
            Self::Or(args) => {
                let mut result = Some(false);
                for p in args {
                    if result == Some(true) {
                        break;
                    }
                    result = match (result, p.evaluate(entity)?) {
                        (_, Some(true)) => Some(true),
                        (None, _) | (_, None) => None,
                        _ => Some(false),
                    };
                }
                result
            }
        })
    }

    pub(crate) fn matches(&self, entity: &CachedEntity) -> Result<bool, RuntimeError> {
        Ok(self.evaluate(entity)? == Some(true))
    }
}

pub(crate) fn filter_entities_by_predicate(
    entities: Vec<CachedEntity>,
    pred: &plasm_core::Predicate,
    cgs: &CGS,
) -> Result<Vec<CachedEntity>, RuntimeError> {
    let Some(first) = entities.first() else {
        return Ok(entities);
    };
    let prepared = PreparedEntityPredicate::new(cgs, first.reference.entity_type.as_str(), pred)?;
    let mut out = Vec::with_capacity(entities.len());
    for entity in entities {
        if prepared.matches(&entity)? {
            out.push(entity);
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
    capability
        .input_parameter_names()
        .map(str::to_owned)
        .collect()
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

#[cfg(test)]
mod field_demand_tests {
    use super::*;

    fn client_side_predicate_matches(
        entity: &CachedEntity,
        predicate: &plasm_core::Predicate,
        cgs: &CGS,
    ) -> Result<bool, RuntimeError> {
        PreparedEntityPredicate::new(cgs, entity.reference.entity_type.as_str(), predicate)?
            .matches(entity)
    }

    #[test]
    fn unavailable_fields_fail_when_consumed_without_erasing_membership() {
        let cgs = plasm_core::load_schema_dir(
            &std::path::PathBuf::from(crate::test_manifest_dir())
                .join("../../fixtures/schemas/plasm_pagination_matrix"),
        )
        .unwrap();
        let mut entity = CachedEntity::from_decoded(
            Ref::new("Item", "one"),
            Default::default(),
            Default::default(),
            1,
            EntityCompleteness::Summary,
        );
        entity.unavailable_fields.insert("n".into());
        let predicate = plasm_core::Predicate::eq("n", 42);
        let error = client_side_predicate_matches(&entity, &predicate, &cgs).unwrap_err();
        assert!(matches!(error, RuntimeError::FieldUnavailable { .. }));
        let failure = crate::ExecutionFailure::from(error);
        assert_eq!(failure.code, "field_unavailable");
        assert_eq!(failure.cause, crate::FailureCause::ResponseContract);
        assert_eq!(failure.recovery, crate::RecoveryDisposition::Stop);
        assert!(client_side_predicate_matches(
            &entity,
            &plasm_core::Predicate::Or {
                args: vec![plasm_core::Predicate::True, predicate.clone()],
            },
            &cgs
        )
        .unwrap());
        assert!(!client_side_predicate_matches(
            &entity,
            &plasm_core::Predicate::And {
                args: vec![plasm_core::Predicate::False, predicate],
            },
            &cgs
        )
        .unwrap());
        let bound = crate::row_predicate::BoundRowPredicate {
            field_path: plasm_core::FieldPath::from_dotted("n").unwrap(),
            op: plasm_core::PlanPredicateOp::Eq,
            value: plasm_core::operand_binding::ResolvedValue::new(Value::Integer(42)).unwrap(),
        };
        assert!(matches!(
            crate::row_predicate::PreparedRowPredicate::prepare(&cgs, "Item", &bound)
                .unwrap()
                .matches_entity(&entity),
            Err(RuntimeError::FieldUnavailable { .. })
        ));
        let mut heap = crate::top_k::TopKHeap::new(
            crate::top_k::TopKSpec {
                count: 1,
                sort_key: vec!["n".into()],
                descending: false,
                row_filter: vec![],
            },
            plasm_core::value_contract::ValueContract::scalar(plasm_core::FieldType::Integer),
            vec![],
        )
        .unwrap();
        assert!(matches!(
            heap.insert(crate::paginated_collect::RetainedObservation {
                entity,
                descendants: vec![]
            }),
            Err(RuntimeError::FieldUnavailable { .. })
        ));
    }

    #[test]
    fn residual_not_preserves_null_as_unknown() {
        let cgs = plasm_core::load_schema_dir(
            &std::path::PathBuf::from(crate::test_manifest_dir())
                .join("../../fixtures/schemas/plasm_pagination_matrix"),
        )
        .unwrap();
        let entity = CachedEntity::from_decoded(
            Ref::new("Item", "one"),
            [("n".into(), Value::Null)].into_iter().collect(),
            Default::default(),
            1,
            EntityCompleteness::Summary,
        );
        let predicate = plasm_core::Predicate::Not {
            predicate: Box::new(plasm_core::Predicate::eq("n", 42)),
        };
        assert!(!PreparedEntityPredicate::new(&cgs, "Item", &predicate)
            .unwrap()
            .matches(&entity)
            .unwrap());
    }
}
