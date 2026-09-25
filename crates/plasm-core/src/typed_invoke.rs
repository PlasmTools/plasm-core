//! Structured invoke/create inputs aligned with [`crate::schema::InputType`] (typed IR migration).

use crate::schema::{
    input_variant_body_type, union_variant_constructor_symbol, InputFieldSchema, InputFieldWire,
    InputType, InputVariantSchema, CGS,
};
use crate::typed_literal::TypedLiteral;
use crate::value::{PlasmInputRef, Value};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Structured invoke body after lowering from [`Value`] using an [`InputType`] description.
#[derive(Debug, Clone, PartialEq)]
pub enum TypedInvokeInput {
    Leaf(TypedLiteral),
    PlasmInputRef(PlasmInputRef),
    Array(Vec<TypedInvokeInput>),
    Object {
        fields: IndexMap<String, TypedInvokeInput>,
        /// Extra keys when `additional_fields` is true on the object schema.
        #[allow(clippy::option_option)]
        extra: Option<IndexMap<String, Value>>,
    },
    /// Preserves arbitrary JSON subtrees (`FieldType::Json`, unstructured blobs).
    Json(Value),
    Union {
        variant_index: usize,
        /// Wire discriminator merged into the lowered object (CML / HTTP JSON).
        wire_field: String,
        wire_value: String,
        value: Box<TypedInvokeInput>,
        /// Logical field name → path segments under the wire object (excluding the discriminator).
        nested_wire_paths: IndexMap<String, Vec<String>>,
        /// Logical array field name → JSON key wrapping each array element on the wire.
        array_element_wrap_keys: IndexMap<String, String>,
    },
}

impl TypedInvokeInput {
    /// Validate a logical tagged record before applying any transport field remapping.
    /// In particular, do not discard fields from another union alternative.
    pub fn from_union_variant(
        variant: &InputVariantSchema,
        variant_index: usize,
        fields: Value,
        cgs: &CGS,
    ) -> Result<Self, String> {
        let body_type = input_variant_body_type(variant);
        if let Value::Object(object) = &fields {
            for field in &variant.fields {
                if field
                    .wire_array_element_key
                    .as_ref()
                    .is_some_and(|key| !key.is_empty())
                    && object
                        .get(&field.name)
                        .is_some_and(|value| matches!(value, Value::PlasmInputRef(_)))
                {
                    return Err(format!(
                        "union field {} requires an explicit array for per-element wire wrapping",
                        field.name
                    ));
                }
            }
        }
        crate::capability_input::validate_input_type(&fields, &body_type, &variant.name, cgs)
            .map_err(|error| error.to_string())?;
        let inner = lift_inner(&fields, &body_type, cgs)
            .map_err(|()| format!("cannot lower union variant {}", variant.name))?;
        let (nested_wire_paths, array_element_wrap_keys) = union_merge_hints_from_variant(variant);
        Ok(Self::Union {
            variant_index,
            wire_field: variant.wire.field.clone(),
            wire_value: variant.wire.value.clone(),
            value: Box::new(inner),
            nested_wire_paths,
            array_element_wrap_keys,
        })
    }

    /// Materialize back to wire [`Value`] for CML / HTTP layers.
    pub fn to_value(&self) -> Value {
        match self {
            TypedInvokeInput::Leaf(t) => t.to_value(),
            TypedInvokeInput::PlasmInputRef(r) => Value::PlasmInputRef(r.clone()),
            TypedInvokeInput::Array(items) => {
                Value::Array(items.iter().map(Self::to_value).collect())
            }
            TypedInvokeInput::Object { fields, extra } => {
                let mut m: IndexMap<String, Value> = fields
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect();
                if let Some(ex) = extra {
                    for (k, v) in ex {
                        m.insert(k.clone(), v.clone());
                    }
                }
                Value::Object(m)
            }
            TypedInvokeInput::Json(v) => v.clone(),
            TypedInvokeInput::Union {
                wire_field,
                wire_value,
                value,
                nested_wire_paths,
                array_element_wrap_keys,
                ..
            } => {
                let mut m = IndexMap::new();
                m.insert(wire_field.clone(), Value::String(wire_value.clone()));
                match value.as_ref() {
                    TypedInvokeInput::Object { fields, extra } => {
                        for (k, v) in fields {
                            let mut val = v.to_value();
                            if let Some(ek) = array_element_wrap_keys.get(k) {
                                // Non-array values remain visible to validation; never erase them.
                                val = match val {
                                    Value::Array(items) => Value::Array(
                                        items
                                            .into_iter()
                                            .map(|elem| {
                                                Value::Object(IndexMap::from([(ek.clone(), elem)]))
                                            })
                                            .collect(),
                                    ),
                                    value => value,
                                };
                            }
                            if let Some(path) = nested_wire_paths.get(k) {
                                if !path.is_empty() {
                                    insert_nested_json_value(&mut m, path, val);
                                } else {
                                    m.insert(k.clone(), val);
                                }
                            } else {
                                m.insert(k.clone(), val);
                            }
                        }
                        if let Some(ex) = extra {
                            for (k, v) in ex {
                                m.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    other => {
                        m.insert("_payload".to_string(), other.to_value());
                    }
                }
                Value::Object(m)
            }
        }
    }
}

/// Invoke/create capability input: lowered structured form when possible, else raw [`Value`].
#[derive(Debug, Clone, PartialEq)]
pub enum InvokeInputPayload {
    Typed(TypedInvokeInput),
    Raw(Value),
}

impl InvokeInputPayload {
    #[must_use]
    pub fn raw(v: Value) -> Self {
        Self::Raw(v)
    }

    #[must_use]
    pub fn typed(t: TypedInvokeInput) -> Self {
        Self::Typed(t)
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            InvokeInputPayload::Typed(t) => t.to_value(),
            InvokeInputPayload::Raw(v) => v.clone(),
        }
    }

    /// Lift from a validated [`Value`] using the capability input schema root [`InputType`].
    pub fn lift(value: &Value, input_type: &InputType, cgs: &CGS) -> Self {
        match lift_inner(value, input_type, cgs) {
            Ok(t) => Self::Typed(t),
            Err(_) => Self::Raw(value.clone()),
        }
    }
}

impl From<Value> for InvokeInputPayload {
    fn from(value: Value) -> Self {
        Self::Raw(value)
    }
}

impl Serialize for InvokeInputPayload {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for InvokeInputPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = Value::deserialize(deserializer)?;
        Ok(InvokeInputPayload::Raw(v))
    }
}

fn union_merge_hints_from_variant(
    variant: &InputVariantSchema,
) -> (IndexMap<String, Vec<String>>, IndexMap<String, String>) {
    let mut nested = IndexMap::new();
    let mut arr_wrap = IndexMap::new();
    for f in &variant.fields {
        if let Some(path) = f.wire_json_path.as_ref().filter(|p| !p.is_empty()) {
            nested.insert(f.name.clone(), path.clone());
        }
        if let Some(k) = f
            .wire_array_element_key
            .as_ref()
            .cloned()
            .filter(|s| !s.is_empty())
        {
            arr_wrap.insert(f.name.clone(), k);
        }
    }
    (nested, arr_wrap)
}

pub(crate) fn union_variant_needs_wire_decode(variant: &InputVariantSchema) -> bool {
    needs_wire_to_logical_transform(variant)
}

/// Decode a wire-shaped union variant body (after removing the discriminator field) into the logical
/// object keys agents use in [`Value::UnionCtor`] (`markdown`, flat `blocks` arrays, …).
pub(crate) fn logical_object_from_wire_union_body(
    stripped: &IndexMap<String, Value>,
    variant: &InputVariantSchema,
) -> Result<Value, ()> {
    lift_wire_shape_to_logical_object(stripped, variant)
}

fn needs_wire_to_logical_transform(variant: &InputVariantSchema) -> bool {
    variant.fields.iter().any(|f| {
        f.wire_json_path.as_ref().is_some_and(|p| !p.is_empty())
            || f.wire_array_element_key
                .as_ref()
                .is_some_and(|s| !s.is_empty())
    })
}

fn lift_wire_shape_to_logical_object(
    wire_obj: &IndexMap<String, Value>,
    variant: &InputVariantSchema,
) -> Result<Value, ()> {
    let mut remaining = wire_obj.clone();
    let mut logical = IndexMap::new();
    for field in &variant.fields {
        let path = field
            .wire_json_path
            .as_deref()
            .filter(|path| !path.is_empty());
        let value = match path {
            Some(path) => take_wire_path(&mut remaining, path),
            None => remaining.shift_remove(&field.name),
        };
        let Some(mut value) = value else {
            if field.required {
                return Err(());
            }
            continue;
        };
        {
            if let Some(key) = field
                .wire_array_element_key
                .as_ref()
                .filter(|key| !key.is_empty())
            {
                let Value::Array(items) = value else {
                    return Err(());
                };
                value = Value::Array(
                    items
                        .into_iter()
                        .map(|item| {
                            let Value::Object(mut object) = item else {
                                return Err(());
                            };
                            if object.len() != 1 {
                                return Err(());
                            }
                            object.shift_remove(key).ok_or(())
                        })
                        .collect::<Result<_, _>>()?,
                );
            }
        }
        logical.insert(field.name.clone(), value);
    }
    if !remaining.is_empty() {
        return Err(());
    }
    Ok(Value::Object(logical))
}

fn take_wire_path(object: &mut IndexMap<String, Value>, path: &[String]) -> Option<Value> {
    let (head, tail) = path.split_first()?;
    if tail.is_empty() {
        return object.shift_remove(head);
    }
    let Value::Object(child) = object.get_mut(head)? else {
        return None;
    };
    let value = take_wire_path(child, tail);
    if child.is_empty() {
        object.shift_remove(head);
    }
    value
}

fn insert_nested_json_value(root: &mut IndexMap<String, Value>, path: &[String], leaf: Value) {
    debug_assert!(!path.is_empty(), "wire_json_path must be non-empty");
    if path.len() == 1 {
        root.insert(path[0].clone(), leaf);
        return;
    }
    let head = path[0].clone();
    let tail = &path[1..];
    let entry = root
        .entry(head)
        .or_insert_with(|| Value::Object(IndexMap::new()));
    match entry {
        Value::Object(obj_mut) => insert_nested_json_value(obj_mut, tail, leaf),
        _ => panic!("wire_json_path conflict: expected object at {}", path[0]),
    }
}

fn lift_inner(value: &Value, input_type: &InputType, cgs: &CGS) -> Result<TypedInvokeInput, ()> {
    if matches!(value, Value::PlasmInputRef(_)) {
        let r = match value {
            Value::PlasmInputRef(r) => r.clone(),
            _ => unreachable!(),
        };
        return Ok(TypedInvokeInput::PlasmInputRef(r));
    }
    if matches!(value, Value::GetScalarExtract(_)) {
        return Ok(TypedInvokeInput::Json(value.clone()));
    }

    match input_type {
        InputType::None => {
            if matches!(value, Value::Null) {
                Ok(TypedInvokeInput::Leaf(TypedLiteral::Null))
            } else {
                Err(())
            }
        }
        InputType::Value {
            field_type,
            allowed_values: _,
        } => {
            use crate::FieldType;
            if matches!(field_type, FieldType::Json) {
                let v = match value {
                    Value::String(ref s) => {
                        if let Some(parsed) = crate::value::parse_json_subtree_str(s) {
                            parsed
                        } else {
                            value.clone()
                        }
                    }
                    _ => value.clone(),
                };
                return Ok(TypedInvokeInput::Json(v));
            }
            let lit = TypedLiteral::try_from_value(value).map_err(|_| ())?;
            Ok(TypedInvokeInput::Leaf(lit))
        }
        InputType::Object {
            fields,
            additional_fields,
        } => {
            let obj = value.as_object().ok_or(())?;
            // A payload-only schema must not erase scope or another input lane.
            // The caller retains the original complete invocation when it cannot
            // be represented by this particular typed schema.
            if !additional_fields
                && obj
                    .keys()
                    .any(|key| !fields.iter().any(|field| &field.name == key))
            {
                return Err(());
            }
            let mut out: IndexMap<String, TypedInvokeInput> = IndexMap::new();
            for f in fields {
                match obj.get(&f.name) {
                    Some(fv) => {
                        let nested_ty = field_input_schema_to_input_type(f, cgs)?;
                        out.insert(f.name.clone(), lift_inner(fv, &nested_ty, cgs)?);
                    }
                    None => {
                        if f.required {
                            return Err(());
                        }
                    }
                }
            }
            let extra = if *additional_fields {
                let defined: std::collections::HashSet<_> =
                    fields.iter().map(|x| x.name.as_str()).collect();
                let mut rest = IndexMap::new();
                for (k, v) in obj.iter() {
                    if !defined.contains(k.as_str()) {
                        rest.insert(k.clone(), v.clone());
                    }
                }
                if rest.is_empty() {
                    None
                } else {
                    Some(rest)
                }
            } else {
                None
            };
            Ok(TypedInvokeInput::Object { fields: out, extra })
        }
        InputType::Array {
            element_type,
            min_length: _,
            max_length: _,
        } => {
            let arr = value.as_array().ok_or(())?;
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                out.push(lift_inner(item, element_type, cgs)?);
            }
            Ok(TypedInvokeInput::Array(out))
        }
        InputType::Union { variants } => {
            if let Value::UnionCtor {
                ctor_label,
                ctor_fields,
            } = value
            {
                let idx = variants
                    .iter()
                    .position(|v| {
                        union_variant_constructor_symbol(v)
                            .is_some_and(|s| s == ctor_label.as_str())
                    })
                    .ok_or(())?;
                let variant = &variants[idx];
                let body_ty = input_variant_body_type(variant);
                let inner = lift_inner(&Value::Object(ctor_fields.clone()), &body_ty, cgs)?;
                let (nested_wire_paths, array_element_wrap_keys) =
                    union_merge_hints_from_variant(variant);
                return Ok(TypedInvokeInput::Union {
                    variant_index: idx,
                    wire_field: variant.wire.field.clone(),
                    wire_value: variant.wire.value.clone(),
                    value: Box::new(inner),
                    nested_wire_paths,
                    array_element_wrap_keys,
                });
            }
            if let Value::Object(obj) = value {
                for (i, variant) in variants.iter().enumerate() {
                    let wf = variant.wire.field.as_str();
                    if let Some(Value::String(disc)) = obj.get(wf) {
                        if disc.as_str() == variant.wire.value.as_str() {
                            let mut stripped = obj.clone();
                            stripped.shift_remove(wf);
                            let body_ty = input_variant_body_type(variant);
                            let logical_val = if needs_wire_to_logical_transform(variant) {
                                lift_wire_shape_to_logical_object(&stripped, variant)?
                            } else {
                                Value::Object(stripped)
                            };
                            if let Ok(inner) = lift_inner(&logical_val, &body_ty, cgs) {
                                let (nested_wire_paths, array_element_wrap_keys) =
                                    union_merge_hints_from_variant(variant);
                                return Ok(TypedInvokeInput::Union {
                                    variant_index: i,
                                    wire_field: variant.wire.field.clone(),
                                    wire_value: variant.wire.value.clone(),
                                    value: Box::new(inner),
                                    nested_wire_paths,
                                    array_element_wrap_keys,
                                });
                            }
                        }
                    }
                }
            }
            Err(())
        }
    }
}

fn field_input_schema_to_input_type(f: &InputFieldSchema, cgs: &CGS) -> Result<InputType, ()> {
    match &f.wire {
        InputFieldWire::Inline(ty) => Ok((**ty).clone()),
        InputFieldWire::Registry(_) => {
            named_input_type(f.named_value(cgs).map_err(|_| ())?, cgs, 0)
        }
    }
}

fn named_input_type(
    value: &crate::schema::NamedValueSchema,
    cgs: &CGS,
    depth: usize,
) -> Result<InputType, ()> {
    if depth >= 64 {
        return Err(());
    }
    if value.field_type == crate::FieldType::Array {
        let item = value.array_items.as_ref().ok_or(())?;
        let element = cgs
            .values
            .get(item.kind.registry_key().as_str())
            .ok_or(())?;
        Ok(InputType::Array {
            element_type: Box::new(named_input_type(element, cgs, depth + 1)?),
            min_length: value.domain.constraints.min_length,
            max_length: value.domain.constraints.max_length,
        })
    } else {
        Ok(InputType::Value {
            field_type: value.field_type.clone(),
            allowed_values: value.allowed_values.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{InputFieldWire, InputType, NamedValueSchema, ValueDomainKey};
    use crate::FieldType;
    use crate::Value;

    #[test]
    fn lifts_simple_object() {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "typed_invoke_title".into(),
            NamedValueSchema {
                domain: Default::default(),
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                array_items: None,
                currency: None,
            },
        );
        let input_type = InputType::Object {
            fields: vec![InputFieldSchema {
                name: "title".into(),
                wire: InputFieldWire::Registry(
                    ValueDomainKey::new("typed_invoke_title").expect("key"),
                ),
                required: true,
                description: None,
                default: None,
                wire_json_path: None,
                wire_array_element_key: None,
                sink_class: None,
            }],
            additional_fields: false,
        };
        let v = Value::Object({
            let mut m = IndexMap::new();
            m.insert("title".into(), Value::String("hi".into()));
            m
        });
        let p = InvokeInputPayload::lift(&v, &input_type, &cgs);
        match p {
            InvokeInputPayload::Typed(TypedInvokeInput::Object { fields, .. }) => {
                assert!(fields.contains_key("title"));
            }
            other => panic!("expected typed object: {other:?}"),
        }
    }

    #[test]
    fn union_wire_decode_retains_common_lanes_and_optional_nested_fields() {
        let cgs = crate::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_union_matrix"),
        )
        .unwrap();
        let schema = &cgs.capabilities["record_write"]
            .inputs
            .payload
            .as_ref()
            .unwrap()
            .input_type;
        for json in [
            serde_json::json!({"kind":"text","content":{"text":"hello"}}),
            serde_json::json!({"kind":"text","content":{"text":"hello","note":"memo"}}),
            serde_json::json!({"kind":"text","content":{"text":"hello"},"tenant":"t1","request_id":"req1"}),
            serde_json::json!({"kind":"text","content":{"text":"hello","unexpected":"keep"}}),
        ] {
            let value: Value = serde_json::from_value(json).unwrap();
            assert_eq!(
                InvokeInputPayload::lift(&value, schema, &cgs).to_value(),
                value
            );
        }
        let InputType::Union { variants } = schema else {
            panic!("union fixture")
        };
        let fields = serde_json::from_value(serde_json::json!({"text":"hello","count":1})).unwrap();
        assert!(TypedInvokeInput::from_union_variant(&variants[0], 0, fields, &cgs).is_err());
    }

    #[test]
    fn union_array_wrapping_composes_with_paths_without_erasing_deferred_values() {
        let cgs = crate::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_union_matrix"),
        )
        .unwrap();
        let InputType::Union { variants } = &cgs.capabilities["record_write"]
            .inputs
            .payload
            .as_ref()
            .unwrap()
            .input_type
        else {
            panic!("union fixture")
        };
        let mut variant = variants[1].clone();
        let labels = variant
            .fields
            .iter_mut()
            .find(|field| field.name == "labels")
            .unwrap();
        labels.wire_array_element_key = Some("label".into());
        labels.wire_json_path = Some(vec!["content".into(), "labels".into()]);
        let logical: Value =
            serde_json::from_value(serde_json::json!({"count":2,"labels":["red","blue"]})).unwrap();
        let typed = TypedInvokeInput::from_union_variant(&variant, 1, logical, &cgs).unwrap();
        let wire = typed.to_value();
        let expected: Value = serde_json::from_value(serde_json::json!({"kind":"count","count":2,"content":{"labels":[{"label":"red"},{"label":"blue"}]}})).unwrap();
        assert_eq!(wire, expected);
        assert_eq!(
            InvokeInputPayload::lift(
                &wire,
                &InputType::Union {
                    variants: vec![variant.clone()]
                },
                &cgs
            )
            .to_value(),
            wire
        );
        let reference = Value::PlasmInputRef(PlasmInputRef::node_output("labels", vec![]));
        let deferred = Value::Object(IndexMap::from([
            ("count".into(), Value::Integer(2)),
            ("labels".into(), reference),
        ]));
        assert!(
            TypedInvokeInput::from_union_variant(&variant, 1, deferred, &cgs)
                .unwrap_err()
                .contains("explicit array")
        );
    }
}
