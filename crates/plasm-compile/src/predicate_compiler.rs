use crate::{BackendFilter, BackendOp, CompileError};
use indexmap::IndexMap;
use plasm_core::{
    normalize, resolve_query_capability, type_check_predicate, EntityDef, Predicate, CGS,
};

/// Compile a row predicate to a backend filter.
pub fn compile_predicate(
    predicate: &Predicate,
    entity: &EntityDef,
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    // Normalize first
    let normalized = normalize(predicate.clone())?;

    type_check_predicate(&normalized, entity, &[], cgs)?;

    compile_predicate_internal(&normalized, entity, cgs)
}

fn compile_predicate_internal(
    predicate: &Predicate,
    entity: &EntityDef,
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    enum Work<'a> {
        Visit(&'a Predicate, &'a EntityDef),
        Join(bool, usize),
        Not,
        Relation(&'a str, bool),
    }
    let mut pending = vec![Work::Visit(predicate, entity)];
    let mut values = Vec::new();
    while let Some(step) = pending.pop() {
        match step {
            Work::Visit(predicate, entity) => match predicate {
                Predicate::True => values.push(BackendFilter::True),
                Predicate::False => values.push(BackendFilter::False),
                Predicate::Comparison { field, op, value } => {
                    values.push(compile_comparison(field, *op, value, entity, cgs)?)
                }
                Predicate::And { args } | Predicate::Or { args } => {
                    pending.push(Work::Join(
                        matches!(predicate, Predicate::And { .. }),
                        args.len(),
                    ));
                    pending.extend(args.iter().rev().map(|arg| Work::Visit(arg, entity)));
                }
                Predicate::Not { predicate } => {
                    pending.push(Work::Not);
                    pending.push(Work::Visit(predicate, entity));
                }
                Predicate::ExistsRelation {
                    relation,
                    predicate,
                } => {
                    let target = relation_target(relation, entity, cgs)?;
                    pending.push(Work::Relation(relation, predicate.is_some()));
                    if let Some(child) = predicate {
                        pending.push(Work::Visit(child, target));
                    }
                }
            },
            Work::Join(and, count) => {
                let filters = values.split_off(values.len() - count);
                values.push(
                    if and {
                        BackendFilter::and(filters)
                    } else {
                        BackendFilter::or(filters)
                    }
                    .simplify_shallow(),
                );
            }
            Work::Not => {
                let child = values.pop().expect("compiled predicate child");
                values.push(BackendFilter::negate(child).simplify_shallow());
            }
            Work::Relation(relation, child) => {
                let filter = child.then(|| values.pop().expect("compiled relation child"));
                values.push(BackendFilter::relation(relation, filter));
            }
        }
    }
    Ok(values.pop().expect("compiled predicate root"))
}

fn compile_comparison(
    field: &str,
    op: plasm_core::CompOp,
    value: &plasm_core::TypedComparisonValue,
    entity: &EntityDef,
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    let mut wire = value.to_value();
    if let Some(schema) = entity.fields.get(field) {
        let domain = schema.named_value(cgs).map_err(CompileError::from)?;
        plasm_core::temporal_input::encode_domain_temporals(&mut wire, domain, cgs)?;
        return Ok(BackendFilter::field(field, BackendOp::from(op), wire));
    }

    Err(plasm_core::TypeError::FieldNotFound {
        field: field.to_owned(),
        entity: entity.name.to_string(),
    }
    .into())
}

fn relation_target<'a>(
    relation: &str,
    entity: &EntityDef,
    cgs: &'a CGS,
) -> Result<&'a EntityDef, CompileError> {
    let schema =
        entity
            .relations
            .get(relation)
            .ok_or_else(|| plasm_core::TypeError::RelationNotFound {
                relation: relation.to_owned(),
                entity: entity.name.to_string(),
            })?;
    cgs.get_entity(&schema.target_resource).ok_or_else(|| {
        plasm_core::TypeError::EntityNotFound {
            entity: schema.target_resource.to_string(),
        }
        .into()
    })
}

/// Compile a query expression to a backend filter.
///
/// The resulting `BackendFilter` covers only the resolved row predicate.
pub fn compile_query(
    query: &plasm_core::QueryExpr,
    cgs: &CGS,
) -> Result<Option<BackendFilter>, CompileError> {
    Ok(resolve_query(query, cgs)?.filter)
}

/// One resolution of request bindings and the row predicate left after source selection.
pub struct ResolvedQuery<'a> {
    pub capability: &'a plasm_core::CapabilitySchema,
    pub bindings: IndexMap<String, plasm_core::Value>,
    pub residual: Option<Predicate>,
    pub filter: Option<BackendFilter>,
}

/// Resolve the owning capability and partition conjunctive request bindings from row tests.
/// Inputs under disjunction or negation cannot be represented by a single request.
pub fn resolve_query<'a>(
    query: &plasm_core::QueryExpr,
    cgs: &'a CGS,
) -> Result<ResolvedQuery<'a>, CompileError> {
    let entity =
        cgs.get_entity(&query.entity)
            .ok_or_else(|| plasm_core::TypeError::EntityNotFound {
                entity: query.entity.to_string(),
            })?;
    let capability_name = resolve_query_capability(query, cgs)?.name.clone();
    let capability = cgs
        .get_capability(capability_name.as_str())
        .expect("resolved capability belongs to the catalog");
    let params: Vec<_> = capability.query_surface_fields().cloned().collect();
    let mut values: IndexMap<String, Vec<plasm_core::Value>> = IndexMap::new();
    let residual = if let Some(pred) = &query.predicate {
        type_check_predicate(pred, entity, &params, cgs)?;
        partition_predicate(pred, &capability.inputs, None, &mut values)?
    } else {
        None
    };
    let filter = match (&query.predicate, &residual) {
        (_, Some(predicate)) => Some(compile_predicate(predicate, entity, cgs)?),
        (Some(_), None) => Some(BackendFilter::True),
        (None, None) => None,
    };
    let bindings = values
        .into_iter()
        .filter_map(|(key, mut values)| match values.len() {
            0 => None,
            1 => Some((key, values.remove(0))),
            _ => Some((key, plasm_core::Value::Array(values))),
        })
        .collect();
    Ok(ResolvedQuery {
        capability,
        bindings,
        residual,
        filter,
    })
}

fn partition_predicate(
    predicate: &Predicate,
    inputs: &plasm_core::schema::CapabilityInputs,
    boolean_parent: Option<plasm_core::rowset::SourcePredicateError>,
    values: &mut IndexMap<String, Vec<plasm_core::Value>>,
) -> Result<Option<Predicate>, CompileError> {
    use plasm_core::{CompOp, Value};
    match predicate {
        Predicate::Comparison { field, op, value } => {
            let is_input = inputs.query_source_lane(field).is_some();
            if let (true, Some(reason)) = (is_input, boolean_parent) {
                return Err(plasm_core::TypeError::RowsetNormalize {
                    source: Box::new(plasm_core::rowset::RowsetNormalizeError::InvalidPredicate {
                        reason,
                    }),
                }
                .into());
            }
            // Only declared, conjunctive source inputs become request variables.
            if is_input {
                let rhs = value.to_value();
                if !matches!(rhs, Value::PlasmInputRef(_)) {
                    let slot = values.entry(field.clone()).or_default();
                    match (op, rhs) {
                        (CompOp::In | CompOp::Contains, Value::Array(items)) => slot.extend(items),
                        (CompOp::In | CompOp::Contains, value) => slot.push(value),
                        (_, value) => {
                            slot.clear();
                            slot.push(value);
                        }
                    }
                }
            }
            Ok((!is_input).then(|| predicate.clone()))
        }
        Predicate::And { args } => {
            let mut kept = Vec::new();
            for arg in args {
                if let Some(pred) = partition_predicate(arg, inputs, boolean_parent, values)? {
                    kept.push(pred);
                }
            }
            Ok(match kept.len() {
                0 if boolean_parent.is_some() => Some(Predicate::True),
                0 => None,
                1 => kept.pop(),
                _ => Some(Predicate::And { args: kept }),
            })
        }
        Predicate::Or { args } => {
            let mut kept = Vec::with_capacity(args.len());
            for arg in args {
                if let Some(pred) = partition_predicate(
                    arg,
                    inputs,
                    Some(plasm_core::rowset::SourcePredicateError::Disjunction),
                    values,
                )? {
                    kept.push(pred);
                }
            }
            Ok(Some(Predicate::Or { args: kept }))
        }
        Predicate::Not { predicate } => {
            let child = partition_predicate(
                predicate,
                inputs,
                Some(plasm_core::rowset::SourcePredicateError::Negation),
                values,
            )?
            .expect("row-only negation retains its predicate");
            Ok(Some(Predicate::Not {
                predicate: Box::new(child),
            }))
        }
        Predicate::True | Predicate::False | Predicate::ExistsRelation { .. } => {
            Ok(Some(predicate.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_partition_preserves_row_boolean_logic_and_rejects_input_disjunction() {
        let path = std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/scoped_query_matrix");
        let cgs = plasm_core::loader::load_schema_dir(&path).unwrap();
        let mut query = plasm_core::QueryExpr::all("Member");
        query.capability_name = Some("member_query".into());
        let scope = Predicate::eq("cohort_id", 7_i64);
        let rows = Predicate::or(vec![Predicate::eq("id", "a"), Predicate::eq("id", "b")]);
        query.predicate = Some(Predicate::and(vec![scope.clone(), rows.clone()]));
        let resolved = resolve_query(&query, &cgs).unwrap();
        assert_eq!(resolved.capability.name.as_str(), "member_query");
        assert_eq!(
            resolved.bindings.get("cohort_id"),
            Some(&plasm_core::Value::Integer(7))
        );
        assert!(
            !resolved.bindings.contains_key("id"),
            "an OR cannot be reduced to one request value"
        );
        assert_eq!(resolved.residual, Some(rows));
        assert!(matches!(resolved.filter, Some(BackendFilter::Or { .. })));
        let row = Predicate::eq("id", "a");
        query.predicate = Some(Predicate::and(vec![scope.clone(), row.clone()]));
        let resolved = resolve_query(&query, &cgs).unwrap();
        assert!(!resolved.bindings.contains_key("id"));
        assert_eq!(resolved.residual, Some(row));
        let negated_row = Predicate::negate(Predicate::eq("id", "c"));
        query.predicate = Some(Predicate::and(vec![scope.clone(), negated_row.clone()]));
        let resolved = resolve_query(&query, &cgs).unwrap();
        assert_eq!(resolved.residual, Some(negated_row));
        assert!(matches!(resolved.filter, Some(BackendFilter::Not { .. })));
        for predicate in [
            Predicate::or(vec![scope.clone(), Predicate::eq("id", "a")]),
            Predicate::negate(scope.clone()),
            Predicate::or(vec![scope.clone(), scope.clone()]),
        ] {
            query.predicate = Some(predicate);
            assert!(matches!(
                resolve_query(&query, &cgs),
                Err(CompileError::TypeError { .. })
            ));
        }
    }

    #[test]
    fn required_scope_input_is_transport_data_not_a_row_filter() {
        let path = std::path::Path::new(crate::test_manifest_dir())
            .join("../../fixtures/schemas/scoped_query_matrix");
        let cgs = plasm_core::loader::load_schema_dir(&path).unwrap();
        crate::validate_cgs_capability_templates(&cgs).unwrap();
        for entity in ["Child", "ChildWithParent"] {
            let mut parsed = plasm_core::expr_parser::parse(
                &format!("{entity}{{parent_id=Parent(\"parent-one\")}}"),
                &cgs,
            )
            .unwrap();
            plasm_core::normalize_expr_query_capabilities(&mut parsed.expr, &cgs).unwrap();
            let plasm_core::Expr::Query(query) = parsed.expr else {
                panic!("expected a scoped query");
            };
            assert!(matches!(
                compile_query(&query, &cgs).unwrap(),
                Some(BackendFilter::True)
            ));
        }
        let row_filter = compile_predicate(
            &Predicate::eq("parent_id", "parent-one"),
            cgs.get_entity("ChildWithParent").unwrap(),
            &cgs,
        )
        .unwrap();
        assert!(matches!(row_filter, BackendFilter::Field { .. }));
    }
}
