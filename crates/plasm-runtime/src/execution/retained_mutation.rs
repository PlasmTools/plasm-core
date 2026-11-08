//! Retained mutation source operands at a repair checkpoint.

use plasm_core::entity_ref_value::{EntityRefAtom, EntityRefPayload};
use plasm_core::typed_invoke::{InvokeInputPayload, TypedInvokeInput};
use plasm_core::typed_literal::TypedLiteral;
use plasm_core::{CreateExpr, DeleteExpr, InvokeExpr};
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
    /// Known owned capacity, in addition to the serialized source charge.
    /// Nested dotted-receiver expression capacity is not included.
    pub(super) fn retention_capacity_bytes(&self) -> usize {
        fn input(value: &InvokeInputPayload) -> usize {
            match value {
                InvokeInputPayload::Raw(value) => {
                    crate::materialization::value_retention_capacity(value, 0)
                }
                InvokeInputPayload::Typed(value) => typed_input(value, 0),
            }
        }
        match self {
            Self::Create(expr) => expr
                .capability
                .retention_capacity_bytes()
                .saturating_add(expr.entity.retention_capacity_bytes())
                .saturating_add(
                    expr.catalog_entry_id
                        .as_ref()
                        .map_or(0, |id| id.retention_capacity_bytes()),
                )
                .saturating_add(input(&expr.input))
                .saturating_add(
                    usize::from(expr.dotted_receiver.is_some())
                        .saturating_mul(std::mem::size_of::<plasm_core::Expr>()),
                ),
            Self::Delete(expr) => expr
                .capability
                .retention_capacity_bytes()
                .saturating_add(expr.target.retention_capacity_bytes())
                .saturating_add(
                    expr.catalog_entry_id
                        .as_ref()
                        .map_or(0, |id| id.retention_capacity_bytes()),
                )
                .saturating_add(expr.input.as_ref().map_or(0, input)),
            Self::Invoke(expr) => expr
                .capability
                .retention_capacity_bytes()
                .saturating_add(expr.target.retention_capacity_bytes())
                .saturating_add(
                    expr.catalog_entry_id
                        .as_ref()
                        .map_or(0, |id| id.retention_capacity_bytes()),
                )
                .saturating_add(expr.input.as_ref().map_or(0, input)),
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
    use crate::execution::effect_continuation::MAX_EFFECT_SELECTION_BYTES;

    #[test]
    fn all_mutation_forms_charge_owned_capability_capacity() {
        fn capability() -> plasm_core::CapabilityName {
            let mut name = String::with_capacity(MAX_EFFECT_SELECTION_BYTES + 1);
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
            assert!(source.retention_capacity_bytes() > MAX_EFFECT_SELECTION_BYTES);
        }
    }
}
