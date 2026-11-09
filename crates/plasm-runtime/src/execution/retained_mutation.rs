//! Retained mutation source operands at a repair checkpoint.

use plasm_core::entity_ref_value::{EntityRefAtom, EntityRefPayload};
use plasm_core::typed_invoke::{InvokeInputPayload, TypedInvokeInput};
use plasm_core::typed_literal::TypedLiteral;
use plasm_core::{CreateExpr, DeleteExpr, Expr, InvokeExpr, Predicate};
use serde::Serialize;

#[cfg(test)]
use plasm_core::Value;

/// The caller's original expression, retained for post-dispatch semantics.
#[derive(Serialize)]
pub(super) enum RetainedMutationSource<'a> {
    Create(&'a CreateExpr),
    Delete(&'a DeleteExpr),
    Invoke(&'a InvokeExpr),
}

impl RetainedMutationSource<'_> {
    /// Owned capacity, in addition to the serialized source charge.
    pub(super) fn retention_capacity_bytes(&self) -> usize {
        match self {
            Self::Create(expr) => create(expr, 0),
            Self::Delete(expr) => delete(expr, 0),
            Self::Invoke(expr) => invoke(expr, 0),
        }
    }
}

fn input(value: &InvokeInputPayload, depth: usize) -> usize {
    match value {
        InvokeInputPayload::Raw(value) => {
            crate::materialization::value_retention_capacity(value, depth)
        }
        InvokeInputPayload::Typed(value) => typed_input(value, depth),
    }
}

fn stamp(value: &plasm_core::CatalogEntryStamp) -> usize {
    value.as_ref().map_or(0, |id| id.retention_capacity_bytes())
}

fn create(expr: &CreateExpr, depth: usize) -> usize {
    if depth >= 64 {
        return usize::MAX;
    }
    expr.capability
        .retention_capacity_bytes()
        .saturating_add(expr.entity.retention_capacity_bytes())
        .saturating_add(stamp(&expr.catalog_entry_id))
        .saturating_add(input(&expr.input, depth + 1))
        .saturating_add(expr.dotted_receiver.as_ref().map_or(0, |receiver| {
            std::mem::size_of::<Expr>().saturating_add(expression(receiver, depth + 1))
        }))
}

fn delete(expr: &DeleteExpr, depth: usize) -> usize {
    expr.capability
        .retention_capacity_bytes()
        .saturating_add(expr.target.retention_capacity_bytes())
        .saturating_add(stamp(&expr.catalog_entry_id))
        .saturating_add(
            expr.input
                .as_ref()
                .map_or(0, |value| input(value, depth + 1)),
        )
}

fn invoke(expr: &InvokeExpr, depth: usize) -> usize {
    expr.capability
        .retention_capacity_bytes()
        .saturating_add(expr.target.retention_capacity_bytes())
        .saturating_add(stamp(&expr.catalog_entry_id))
        .saturating_add(
            expr.input
                .as_ref()
                .map_or(0, |value| input(value, depth + 1)),
        )
}

pub(super) fn predicate_retention_capacity_bytes(value: &Predicate) -> usize {
    predicate(value, 0)
}

fn predicate(value: &Predicate, depth: usize) -> usize {
    if depth >= 64 {
        return usize::MAX;
    }
    match value {
        Predicate::True | Predicate::False => 0,
        Predicate::Comparison { field, value, .. } => {
            field
                .capacity()
                .saturating_add(value.typed_literal().map_or_else(
                    || {
                        value.dynamic_value().map_or(0, |value| {
                            crate::materialization::value_retention_capacity(value, depth + 1)
                        })
                    },
                    |value| typed_literal(value, depth + 1),
                ))
        }
        Predicate::And { args } | Predicate::Or { args } => args.iter().fold(
            args.capacity()
                .saturating_mul(std::mem::size_of::<Predicate>()),
            |sum, value| sum.saturating_add(predicate(value, depth + 1)),
        ),
        Predicate::Not { predicate: inner } => {
            std::mem::size_of::<Predicate>().saturating_add(predicate(inner, depth + 1))
        }
        Predicate::ExistsRelation {
            relation,
            predicate: inner,
        } => relation
            .capacity()
            .saturating_add(inner.as_ref().map_or(0, |value| {
                std::mem::size_of::<Predicate>().saturating_add(predicate(value, depth + 1))
            })),
    }
}

/// Conservatively count the heap capacity retained by an executable expression.
/// Expressions exceeding the traversal depth fail closed with `usize::MAX`.
pub fn expression_retention_capacity_bytes(value: &Expr) -> usize {
    expression(value, 0)
}

fn expression(value: &Expr, depth: usize) -> usize {
    if depth >= 64 {
        return usize::MAX;
    }
    match value {
        Expr::Query(query) => {
            let projection = query.projection.as_ref().map_or(0, |fields| {
                fields.iter().fold(
                    fields
                        .capacity()
                        .saturating_mul(std::mem::size_of::<String>()),
                    |sum, field| sum.saturating_add(field.capacity()),
                )
            });
            query
                .entity
                .retention_capacity_bytes()
                .saturating_add(stamp(&query.catalog_entry_id))
                .saturating_add(
                    query
                        .capability_name
                        .as_ref()
                        .map_or(0, |name| name.retention_capacity_bytes()),
                )
                .saturating_add(
                    query
                        .predicate
                        .as_ref()
                        .map_or(0, |value| predicate(value, depth + 1)),
                )
                .saturating_add(
                    query
                        .pagination
                        .as_ref()
                        .and_then(|page| page.cursor.as_ref())
                        .map_or(0, String::capacity),
                )
                .saturating_add(projection)
        }
        Expr::Get(get) => get
            .reference
            .retention_capacity_bytes()
            .saturating_add(stamp(&get.catalog_entry_id))
            .saturating_add(
                get.capability_name
                    .as_ref()
                    .map_or(0, |name| name.retention_capacity_bytes()),
            )
            .saturating_add(
                get.input
                    .as_ref()
                    .map_or(0, |value| input(value, depth + 1)),
            ),
        Expr::Create(value) => create(value, depth + 1),
        Expr::Delete(value) => delete(value, depth + 1),
        Expr::Invoke(value) => invoke(value, depth + 1),
        Expr::Chain(chain) => {
            let base = chain
                .selector
                .capacity()
                .saturating_add(stamp(&chain.catalog_entry_id))
                .saturating_add(std::mem::size_of::<Expr>())
                .saturating_add(expression(&chain.source, depth + 1));
            match &chain.step {
                plasm_core::expr::ChainStep::AutoGet => base,
                plasm_core::expr::ChainStep::Explicit { expr } => base
                    .saturating_add(std::mem::size_of::<Expr>())
                    .saturating_add(expression(expr, depth + 1)),
            }
        }
        Expr::Page(value) => value.handle.retention_capacity_bytes(),
        Expr::Wait(value) => value.handle.retention_capacity_bytes(),
        Expr::Cancel(value) => value.handle.retention_capacity_bytes(),
        Expr::TeachingValue { value } => {
            crate::materialization::value_retention_capacity(value, depth + 1)
        }
    }
}

fn entity_ref(value: &EntityRefPayload, depth: usize) -> usize {
    if depth >= 64 {
        return usize::MAX;
    }
    match value {
        EntityRefPayload::Atom(EntityRefAtom::String(value)) => value.capacity(),
        EntityRefPayload::Atom(_) => 0,
        EntityRefPayload::Compound(values) => values.iter().fold(
            crate::materialization::retained_index_capacity::<String, EntityRefPayload>(
                values.capacity(),
            ),
            |sum, (key, value)| {
                sum.saturating_add(key.capacity())
                    .saturating_add(entity_ref(value, depth + 1))
            },
        ),
    }
}

fn typed_literal(value: &TypedLiteral, depth: usize) -> usize {
    if depth >= 64 {
        return usize::MAX;
    }
    match value {
        TypedLiteral::String(value) => value.capacity(),
        TypedLiteral::StringTemplate(value) => value.metadata_retention_capacity_bytes(),
        TypedLiteral::Array(values) => values.iter().fold(
            values
                .capacity()
                .saturating_mul(std::mem::size_of::<TypedLiteral>()),
            |sum, value| sum.saturating_add(typed_literal(value, depth + 1)),
        ),
        TypedLiteral::EntityRef(value) => entity_ref(value, depth + 1),
        TypedLiteral::InputRef(value) => {
            crate::materialization::input_reference_retention_capacity(value)
        }
        TypedLiteral::Money(value) => value.retention_capacity_bytes(),
        TypedLiteral::Null
        | TypedLiteral::Bool(_)
        | TypedLiteral::Integer(_)
        | TypedLiteral::Float(_) => 0,
    }
}

fn typed_input(value: &TypedInvokeInput, depth: usize) -> usize {
    if depth >= 64 {
        return usize::MAX;
    }
    match value {
        TypedInvokeInput::Leaf(value) => typed_literal(value, depth + 1),
        TypedInvokeInput::PlasmInputRef(value) => {
            crate::materialization::input_reference_retention_capacity(value)
        }
        TypedInvokeInput::Array(values) => values.iter().fold(
            values
                .capacity()
                .saturating_mul(std::mem::size_of::<TypedInvokeInput>()),
            |sum, value| sum.saturating_add(typed_input(value, depth + 1)),
        ),
        TypedInvokeInput::Object { fields, extra } => {
            let bytes = fields.iter().fold(
                crate::materialization::retained_index_capacity::<String, TypedInvokeInput>(
                    fields.capacity(),
                ),
                |sum, (name, value)| {
                    sum.saturating_add(name.capacity())
                        .saturating_add(typed_input(value, depth + 1))
                },
            );
            bytes.saturating_add(extra.as_ref().map_or(0, |extra| {
                crate::materialization::value_map_capacity(extra, depth + 1)
            }))
        }
        TypedInvokeInput::Json(value) => {
            crate::materialization::value_retention_capacity(value, depth + 1)
        }
        TypedInvokeInput::Union {
            wire_field,
            wire_value,
            value,
            nested_wire_paths,
            array_element_wrap_keys,
            ..
        } => {
            let bytes = wire_field
                .capacity()
                .saturating_add(wire_value.capacity())
                .saturating_add(std::mem::size_of::<TypedInvokeInput>())
                .saturating_add(typed_input(value, depth + 1));
            let bytes = nested_wire_paths.iter().fold(
                bytes.saturating_add(crate::materialization::retained_index_capacity::<
                    String,
                    Vec<String>,
                >(nested_wire_paths.capacity())),
                |sum, (key, path)| {
                    path.iter().fold(
                        sum.saturating_add(key.capacity()).saturating_add(
                            path.capacity()
                                .saturating_mul(std::mem::size_of::<String>()),
                        ),
                        |sum, part| sum.saturating_add(part.capacity()),
                    )
                },
            );
            array_element_wrap_keys.iter().fold(
                bytes.saturating_add(crate::materialization::retained_index_capacity::<
                    String,
                    String,
                >(array_element_wrap_keys.capacity())),
                |sum, (key, value)| {
                    sum.saturating_add(key.capacity())
                        .saturating_add(value.capacity())
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::effect_continuation::MAX_RETAINED_EXECUTION_BYTES;

    #[test]
    fn all_mutation_forms_charge_owned_capability_capacity() {
        fn capability() -> plasm_core::CapabilityName {
            let mut name = String::with_capacity(MAX_RETAINED_EXECUTION_BYTES + 1);
            name.push_str("write");
            name.into()
        }
        let create = CreateExpr {
            capability: capability(),
            entity: "Item".into(),
            input: InvokeInputPayload::raw(Value::Null),
            catalog_entry_id: plasm_core::CatalogEntryStamp::none(),
            dotted_receiver: None,
        };
        let delete = DeleteExpr {
            capability: capability(),
            target: plasm_core::Ref::new("Item", "1"),
            input: None,
            catalog_entry_id: plasm_core::CatalogEntryStamp::none(),
        };
        let invoke = InvokeExpr {
            capability: capability(),
            target: plasm_core::Ref::new("Item", "1"),
            input: None,
            catalog_entry_id: plasm_core::CatalogEntryStamp::none(),
        };
        for source in [
            RetainedMutationSource::Create(&create),
            RetainedMutationSource::Delete(&delete),
            RetainedMutationSource::Invoke(&invoke),
        ] {
            assert!(source.retention_capacity_bytes() > MAX_RETAINED_EXECUTION_BYTES);
        }
    }

    #[test]
    fn nested_receiver_counts_predicate_and_chain_capacity() {
        let mut binding = String::with_capacity(MAX_RETAINED_EXECUTION_BYTES + 1);
        binding.push('x');
        let mut query = plasm_core::QueryExpr::all("Item");
        query.predicate = Some(Predicate::Comparison {
            field: "name".into(),
            op: plasm_core::CompOp::Eq,
            value: plasm_core::typed_literal::TypedComparisonValue::from_value(
                Value::GetScalarExtract(plasm_core::GetScalarExtract {
                    entity: "Item".into(),
                    identity: Box::new(Value::String(binding)),
                    wire: "name".into(),
                    catalog_entry_id: None,
                }),
            ),
        });
        let nested = Expr::Chain(plasm_core::expr::ChainExpr::auto_get(
            Expr::Query(query),
            "child",
        ));
        assert!(expression(&nested, 0) > MAX_RETAINED_EXECUTION_BYTES);

        let mut deep = Expr::Query(plasm_core::QueryExpr::all("Item"));
        for _ in 0..64 {
            deep = Expr::Chain(plasm_core::expr::ChainExpr::auto_get(deep, "child"));
        }
        assert_eq!(expression(&deep, 0), usize::MAX);
    }
}
