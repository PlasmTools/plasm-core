use crate::{BackendFilter, BackendOp, CompileError};
use plasm_core::{
    normalize, resolve_query_capability, type_check_predicate, CapabilityKind, EntityDef,
    InputFieldSchema, Predicate, CGS,
};

/// Compile a predicate to a backend filter.
///
/// Capability parameters that are not entity fields (e.g. `q`, `t`, `s` with
/// `role: search`) are NOT compiled to `BackendFilter` nodes — they go straight
/// into the CML environment via `extract_predicate_vars`. They produce
/// `BackendFilter::True` (no-op filter) here so the BackendFilter only captures
/// genuine entity-field predicates.
pub fn compile_predicate(
    predicate: &Predicate,
    entity: &EntityDef,
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    compile_predicate_with_cap_params(predicate, entity, &[], cgs)
}

/// Internal version that accepts resolved capability parameters.
fn compile_predicate_with_cap_params(
    predicate: &Predicate,
    entity: &EntityDef,
    cap_params: &[InputFieldSchema],
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    // Normalize first
    let normalized = normalize(predicate.clone())?;

    // Type-check using both entity fields and capability parameters
    type_check_predicate(&normalized, entity, cap_params, cgs)?;

    // Compile
    compile_predicate_internal(&normalized, entity, cap_params, cgs)
}

fn compile_predicate_internal(
    predicate: &Predicate,
    entity: &EntityDef,
    cap_params: &[InputFieldSchema],
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    enum Work<'a> {
        Visit(&'a Predicate, &'a EntityDef, &'a [InputFieldSchema]),
        Join(bool, usize),
        Not,
        Relation(&'a str, bool),
    }
    let mut pending = vec![Work::Visit(predicate, entity, cap_params)];
    let mut values = Vec::new();
    while let Some(step) = pending.pop() {
        match step {
            Work::Visit(predicate, entity, cap_params) => match predicate {
                Predicate::True => values.push(BackendFilter::True),
                Predicate::False => values.push(BackendFilter::False),
                Predicate::Comparison { field, op, value } => values.push(compile_comparison(
                    field, *op, value, entity, cap_params, cgs,
                )?),
                Predicate::And { args } | Predicate::Or { args } => {
                    pending.push(Work::Join(
                        matches!(predicate, Predicate::And { .. }),
                        args.len(),
                    ));
                    pending.extend(
                        args.iter()
                            .rev()
                            .map(|arg| Work::Visit(arg, entity, cap_params)),
                    );
                }
                Predicate::Not { predicate } => {
                    pending.push(Work::Not);
                    pending.push(Work::Visit(predicate, entity, cap_params));
                }
                Predicate::ExistsRelation {
                    relation,
                    predicate,
                } => {
                    let target = relation_target(relation, entity, cgs)?;
                    pending.push(Work::Relation(relation, predicate.is_some()));
                    if let Some(child) = predicate {
                        pending.push(Work::Visit(child, target, &[]));
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
    cap_params: &[InputFieldSchema],
    cgs: &CGS,
) -> Result<BackendFilter, CompileError> {
    let mut wire = value.to_value();
    // Declared query inputs belong to the source request even when a returned row
    // has the same field name. Row predicates call this with no capability inputs.
    if cap_params.iter().any(|p| p.name == field) {
        return Ok(BackendFilter::True);
    }
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
/// The resulting `BackendFilter` covers only entity-field predicates.
/// Capability-parameter predicates (search, sort, response_control params)
/// produce `BackendFilter::True` entries and are handled separately by
/// `extract_predicate_vars` → CML env.
pub fn compile_query(
    query: &plasm_core::QueryExpr,
    cgs: &CGS,
) -> Result<Option<BackendFilter>, CompileError> {
    let entity =
        cgs.get_entity(&query.entity)
            .ok_or_else(|| plasm_core::TypeError::EntityNotFound {
                entity: query.entity.to_string(),
            })?;

    // Resolve capability parameters so the compiler can distinguish
    // entity-field predicates from capability-param pass-throughs.
    // Same resolution as runtime execution (`resolve_query_capability`).
    // Fixtures with no Query/Search capabilities (tests) use empty cap params.
    let cap_params: Vec<InputFieldSchema> = if cgs
        .find_capabilities(&query.entity, CapabilityKind::Query)
        .is_empty()
        && cgs
            .find_capabilities(&query.entity, CapabilityKind::Search)
            .is_empty()
    {
        Vec::new()
    } else {
        let cap = resolve_query_capability(query, cgs).map_err(CompileError::QueryResolution)?;
        cap.query_surface_fields().cloned().collect()
    };

    if let Some(predicate) = &query.predicate {
        let filter = compile_predicate_with_cap_params(predicate, entity, &cap_params, cgs)?;
        Ok(Some(filter))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
