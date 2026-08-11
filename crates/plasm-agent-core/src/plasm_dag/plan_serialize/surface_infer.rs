//! Surface contract + synthetic schema inference.

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
    String,
> {
    if let Expr::Chain(_) = expr {
        return Err(
            "internal: relation chains must be lowered before infer_surface_contract".to_string(),
        );
    }

    let (mut kind, entity, mut effect, mut shape) = infer_surface_contract_from_expr(expr)?;
    let qe = if matches!(shape, crate::plasm_plan::ResultShape::Page) {
        if let Some(qe) = expr.qualified_entity_key() {
            QualifiedEntityKey::from(qe)
        } else if let Expr::Page(p) = expr {
            let resume_entity = session
                .peek_synthetic_paging_resume(&p.handle)
                .map(|c| c.entity_type.clone())
                .or_else(|| {
                    session
                        .peek_paging_resume(&p.handle)
                        .map(|r| r.query.entity.to_string())
                })
                .ok_or_else(|| {
                    format!(
                        "page handle `{}` is not registered in this session",
                        p.handle
                    )
                })?;
            let resolving_cgs = crate::catalog_ownership::resolve_cgs_for_entity(
                session,
                resume_entity.as_str(),
                None,
            )?;
            crate::catalog_ownership::resolve_qualified_entity_key(
                session,
                resume_entity.as_str(),
                Some(resolving_cgs),
            )?
        } else {
            return Err(
                "page continuation requires catalog ownership from session e# / binding — not bare wire entity names".to_string(),
            );
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
                format!(
                    "catalog `{}` is not loaded for entity `{}`",
                    qe.entry_id, qe.entity
                )
            })?;
            if let Some(cap) = resolving_cgs.capabilities.get(capability_name.as_str()) {
                if cap.kind == plasm_core::CapabilityKind::Search {
                    kind = PlanNodeKind::Search;
                }
            }
        }
    } else if let Expr::Invoke(invoke) = expr {
        let resolving_cgs = cgs_for_qualified_entity(session, &qe).ok_or_else(|| {
            format!(
                "catalog `{}` is not loaded for entity `{}`",
                qe.entry_id, qe.entity
            )
        })?;
        let cap = resolving_cgs
            .get_capability(invoke.capability.as_str())
            .ok_or_else(|| format!("unknown action capability `{}`", invoke.capability))?;
        (effect, shape) = invoke_effect_and_shape(cap)?;
    }
    Ok((kind, qe, effect, shape))
}

fn invoke_effect_and_shape(
    cap: &CapabilitySchema,
) -> Result<(EffectClass, crate::plasm_plan::ResultShape), String> {
    if cap.effect == Some(plasm_core::CapabilityEffect::Read)
        && cap.output_schema.as_ref().is_some_and(|output| {
            matches!(
                &output.output_type,
                plasm_core::OutputType::SideEffect { .. }
            )
        })
    {
        return Err(format!(
            "capability `{}` cannot combine a read effect with side-effect output",
            cap.name
        ));
    }
    let effect = EffectClass::from(cap.effective_effect());
    if effect != EffectClass::Read {
        return Ok((effect, crate::plasm_plan::ResultShape::SideEffectAck));
    }

    let shape = match cap.output_schema.as_ref().map(|output| &output.output_type) {
        Some(plasm_core::OutputType::Entity { .. })
        | Some(plasm_core::OutputType::Custom { .. })
        | Some(plasm_core::OutputType::Status { .. }) => crate::plasm_plan::ResultShape::Single,
        Some(plasm_core::OutputType::Collection { .. }) => crate::plasm_plan::ResultShape::List,
        Some(plasm_core::OutputType::SideEffect { .. }) => {
            return Err(format!(
                "capability `{}` cannot combine a read effect with side-effect output",
                cap.name
            ));
        }
        None if !cap.provides.is_empty() => crate::plasm_plan::ResultShape::Single,
        None => {
            return Err(format!(
                "action capability `{}` has no modeled response",
                cap.name
            ));
        }
    };
    Ok((effect, shape))
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
    String,
> {
    match expr {
        Expr::TeachingValue { .. } => Err(
            "Expr::TeachingValue is teaching-table-only and cannot appear in execution plans"
                .to_string(),
        ),
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
        Expr::Wait(_) | Expr::Cancel(_) => Err(
            "`wait` / `cancel` are host operation continuations and cannot appear in compiled plan surfaces"
                .to_string(),
        ),
    }
}

pub(in crate::plasm_dag) fn schema_from_output_fields<'a>(
    entity: &str,
    fields: impl Iterator<Item = &'a OutputName>,
    kind: SyntheticValueKind,
) -> SyntheticResultSchema {
    SyntheticResultSchema {
        entity: Some(entity.to_string()),
        fields: fields
            .map(|name| SyntheticFieldSchema {
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
        entity: Some(entity.to_string()),
        fields: aggregates
            .iter()
            .map(|agg| SyntheticFieldSchema {
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
                    name,
                    value_kind: SyntheticValueKind::String,
                    source: None,
                })
        })
        .collect();
    fields.extend(aggregates.iter().map(|agg| SyntheticFieldSchema {
        name: agg.name.clone(),
        value_kind: if agg.function == AggregateFunction::Count {
            SyntheticValueKind::Integer
        } else {
            SyntheticValueKind::Number
        },
        source: None,
    }));
    SyntheticResultSchema {
        entity: Some(entity.to_string()),
        fields,
    }
}
pub(in crate::plasm_dag) fn single_unknown_schema(entity: &str) -> SyntheticResultSchema {
    SyntheticResultSchema {
        entity: Some(entity.to_string()),
        fields: vec![SyntheticFieldSchema {
            name: OutputName::new("value".to_string()).expect("constant non-empty"),
            value_kind: SyntheticValueKind::Unknown,
            source: None,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{CapabilityEffect, OutputSchema, OutputType, SemanticEffect};

    fn read_action(output_type: Option<OutputType>, provides: bool) -> CapabilitySchema {
        let mut cap = CapabilitySchema::minimal_test();
        cap.effect = Some(CapabilityEffect::Read);
        cap.provides = provides.then(|| vec!["id".into()]).unwrap_or_default();
        cap.output_schema = output_type.map(|output_type| OutputSchema {
            output_type,
            decoder: serde_json::json!({}),
            idempotent: false,
            reconcile: None,
        });
        cap
    }

    #[test]
    fn read_action_effect_and_output_matrix_lower_without_ack_shape() {
        let single_outputs = [
            OutputType::Entity {
                entity_type: "TestEntity".into(),
            },
            OutputType::Custom {
                schema: serde_json::json!({"type": "array"}),
            },
            OutputType::Status {
                success_indicators: vec!["ok".into()],
            },
        ];
        for output in single_outputs {
            assert_eq!(
                invoke_effect_and_shape(&read_action(Some(output), false)).expect("lower"),
                (EffectClass::Read, crate::plasm_plan::ResultShape::Single)
            );
        }
        assert_eq!(
            invoke_effect_and_shape(&read_action(
                Some(OutputType::Collection {
                    entity_type: "TestEntity".into(),
                    max_count: None,
                }),
                false,
            ))
            .expect("lower collection"),
            (EffectClass::Read, crate::plasm_plan::ResultShape::List)
        );
        assert_eq!(
            invoke_effect_and_shape(&read_action(None, true)).expect("lower provides-only"),
            (EffectClass::Read, crate::plasm_plan::ResultShape::Single)
        );
        assert!(invoke_effect_and_shape(&read_action(None, false)).is_err());
        assert!(invoke_effect_and_shape(&read_action(
            Some(OutputType::SideEffect {
                description: "changes state".into(),
            }),
            false,
        ))
        .is_err());

        let default_action = CapabilitySchema::minimal_test();
        assert_eq!(
            invoke_effect_and_shape(&default_action).expect("default action"),
            (
                EffectClass::SideEffect,
                crate::plasm_plan::ResultShape::SideEffectAck
            )
        );

        for (semantic, expected, expected_core) in [
            (
                SemanticEffect::Read,
                EffectClass::Read,
                plasm_core::EffectClass::Read,
            ),
            (
                SemanticEffect::Write,
                EffectClass::Write,
                plasm_core::EffectClass::Write,
            ),
            (
                SemanticEffect::SideEffect,
                EffectClass::SideEffect,
                plasm_core::EffectClass::SideEffect,
            ),
        ] {
            assert_eq!(EffectClass::from(semantic), expected);
            assert_eq!(plasm_core::EffectClass::from(semantic), expected_core);
        }
    }
}

pub(in crate::plasm_dag) fn looks_like_plasm_effect_template(rhs: &str) -> bool {
    // Distinguish for-each side effects from `source => { … }` derive. `.m#` (teaching-table methods) and
    // all readable verbs must register here—`.label(`, `.update(`, etc.—not just `.m`.
    rhs.contains(".m")
        || rhs.contains("=>")
        || rhs.contains(".update(")
        || rhs.contains(".create(")
        || rhs.contains(".delete(")
        || rhs.contains(".label(")
        || rhs.contains(".invoke(")
}
