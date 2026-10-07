//! Surface contract + synthetic schema inference.

use super::super::error::DagCompilationError;
use super::super::prelude::*;
use super::super::schema_validate::cgs_for_qualified_entity;

pub(in crate::plasm_dag) fn infer_surface_contract(
    session: &ExecuteSession,
    expr: &Expr,
) -> Result<
    (
        PlanNodeKind,
        QualifiedEntityKey,
        EffectClass,
        crate::plasm_plan::ResultShape,
    ),
    DagCompilationError,
> {
    if let Expr::Chain(_) = expr {
        return Err(DagCompilationError::UnloweredRelationChain);
    }

    let (mut kind, entity, mut effect, mut shape) = infer_surface_contract_from_expr(expr)?;
    let mut qe = if matches!(shape, crate::plasm_plan::ResultShape::Page) {
        if let Some(qe) = expr.qualified_entity_key() {
            QualifiedEntityKey::from(qe)
        } else if let Expr::Page(p) = expr {
            session.paging_qualified_entity(&p.handle).ok_or_else(|| {
                DagCompilationError::UnknownPageHandle {
                    handle: p.handle.to_string(),
                }
            })?
        } else {
            return Err(DagCompilationError::PageOwnershipMissing);
        }
    } else if let Some(qe) = expr.qualified_entity_key() {
        QualifiedEntityKey::from(qe)
    } else {
        let resolving_cgs =
            crate::catalog_ownership::resolve_cgs_for_entity(session, entity.as_str(), None)?;
        crate::catalog_ownership::resolve_qualified_entity_key(
            session,
            entity.as_str(),
            Some(resolving_cgs),
        )?
    };
    if let Expr::Query(q) = expr {
        if let Some(capability_name) = q.capability_name.as_ref() {
            let resolving_cgs = cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
                DagCompilationError::CatalogMissing {
                    entry_id: qe.entry_id.to_string(),
                    entity: qe.entity.to_string(),
                }
            })?;
            if let Some(cap) = resolving_cgs.capabilities.get(capability_name.as_str()) {
                if cap.kind == plasm_core::CapabilityKind::Search {
                    kind = PlanNodeKind::Search;
                }
            }
        }
    }
    // Mutations with declared entity outputs retain that entity and cardinality.
    let mutation_capability = match expr {
        Expr::Invoke(inv) => Some(&inv.capability),
        Expr::Create(create) => Some(&create.capability),
        _ => None,
    };
    if let Some(capability) = mutation_capability {
        let resolving_cgs = cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
            DagCompilationError::CatalogMissing {
                entry_id: qe.entry_id.to_string(),
                entity: qe.entity.to_string(),
            }
        })?;
        if let Some(cap) = resolving_cgs.capabilities.get(capability.as_str()) {
            if let Some((entity, cardinality)) = cap.declared_entity_output() {
                qe.entity = entity.into();
                shape = match cardinality {
                    plasm_core::Cardinality::One => crate::plasm_plan::ResultShape::MutationResult,
                    plasm_core::Cardinality::Many => crate::plasm_plan::ResultShape::List,
                };
                effect = EffectClass::Write;
            } else if !cap.provides.is_empty() {
                shape = crate::plasm_plan::ResultShape::MutationResult;
                effect = EffectClass::Write;
            }
        }
    }
    Ok((kind, qe, effect, shape))
}

pub(in crate::plasm_dag) fn infer_surface_contract_from_expr(
    expr: &Expr,
) -> Result<
    (
        PlanNodeKind,
        String,
        EffectClass,
        crate::plasm_plan::ResultShape,
    ),
    DagCompilationError,
> {
    match expr {
        Expr::TeachingValue { .. } => Err(DagCompilationError::TeachingValueInPlan),
        Expr::Query(q) => Ok((
            PlanNodeKind::Query,
            q.entity.as_str().to_string(),
            EffectClass::Read,
            crate::plasm_plan::ResultShape::List,
        )),
        Expr::Get(g) => Ok((
            PlanNodeKind::Get,
            g.reference.entity_type.as_str().to_string(),
            EffectClass::Read,
            crate::plasm_plan::ResultShape::Single,
        )),
        Expr::Create(c) => Ok((
            PlanNodeKind::Create,
            c.entity.as_str().to_string(),
            EffectClass::Write,
            crate::plasm_plan::ResultShape::MutationResult,
        )),
        Expr::Delete(d) => Ok((
            PlanNodeKind::Delete,
            d.target.entity_type.as_str().to_string(),
            EffectClass::Write,
            crate::plasm_plan::ResultShape::SideEffectAck,
        )),
        Expr::Invoke(i) => Ok((
            PlanNodeKind::Action,
            i.target.entity_type.as_str().to_string(),
            EffectClass::SideEffect,
            crate::plasm_plan::ResultShape::SideEffectAck,
        )),
        Expr::Chain(_) => unreachable!(
            "infer_surface_contract routes Expr::Chain before infer_surface_contract_from_expr"
        ),
        Expr::Page(_) => Ok((
            PlanNodeKind::Query,
            "__page__".to_string(),
            EffectClass::Read,
            crate::plasm_plan::ResultShape::Page,
        )),
        Expr::Wait(_) | Expr::Cancel(_) => Err(DagCompilationError::HostContinuationInPlan),
    }
}

pub(in crate::plasm_dag) fn schema_from_output_fields<'a>(
    entity: &str,
    fields: impl Iterator<Item = &'a OutputName>,
    kind: SyntheticValueKind,
) -> SyntheticResultSchema {
    SyntheticResultSchema {
        optional_fields: Default::default(),
        entity: Some(entity.to_string()),
        fields: fields
            .map(|name| SyntheticFieldSchema {
                value_type: None,
                name: name.clone(),
                value_kind: kind,
                source: None,
            })
            .collect(),
    }
}

pub(in crate::plasm_dag) fn schema_from_aggregates(
    entity: &str,
    aggregates: &[crate::plasm_plan::AggregateSpec],
) -> SyntheticResultSchema {
    SyntheticResultSchema {
        optional_fields: Default::default(),
        entity: Some(entity.to_string()),
        fields: aggregates
            .iter()
            .map(|agg| SyntheticFieldSchema {
                value_type: None,
                name: agg.name.clone(),
                value_kind: if agg.function == AggregateFunction::Count {
                    SyntheticValueKind::Integer
                } else {
                    SyntheticValueKind::Number
                },
                source: None,
            })
            .collect(),
    }
}

pub(in crate::plasm_dag) fn schema_from_group_by(
    entity: &str,
    keys: &[FieldPath],
    aggregates: &[crate::plasm_plan::AggregateSpec],
) -> SyntheticResultSchema {
    let mut fields: Vec<SyntheticFieldSchema> = keys
        .iter()
        .filter_map(|k| {
            OutputName::new(k.dotted())
                .ok()
                .map(|name| SyntheticFieldSchema {
                    value_type: None,
                    name,
                    value_kind: SyntheticValueKind::String,
                    source: None,
                })
        })
        .collect();
    fields.extend(aggregates.iter().map(|agg| SyntheticFieldSchema {
        value_type: None,
        name: agg.name.clone(),
        value_kind: if agg.function == AggregateFunction::Count {
            SyntheticValueKind::Integer
        } else {
            SyntheticValueKind::Number
        },
        source: None,
    }));
    SyntheticResultSchema {
        optional_fields: Default::default(),
        entity: Some(entity.to_string()),
        fields,
    }
}
pub(in crate::plasm_dag) fn single_unknown_schema(entity: &str) -> SyntheticResultSchema {
    SyntheticResultSchema {
        optional_fields: Default::default(),
        entity: Some(entity.to_string()),
        fields: vec![SyntheticFieldSchema {
            value_type: None,
            name: OutputName::new("value".to_string()).expect("constant non-empty"),
            value_kind: SyntheticValueKind::Unknown,
            source: None,
        }],
    }
}
