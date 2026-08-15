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
                            let val = v.to_value();
                            if let Some(ek) = array_element_wrap_keys.get(k) {
                                let arr = val.as_array().cloned().unwrap_or_default();
                                let wrapped: Vec<Value> = arr
                                    .into_iter()
                                    .map(|elem| Value::Object(IndexMap::from([(ek.clone(), elem)])))
                                    .collect();
                                m.insert(k.clone(), Value::Array(wrapped));
                            } else if let Some(path) = nested_wire_paths.get(k) {
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

fn get_value_at_json_path<'a>(
    obj: &'a IndexMap<String, Value>,
    path: &[String],
) -> Option<&'a Value> {
    let mut cur = obj;
    let len = path.len();
    for (i, seg) in path.iter().enumerate() {
        let v = cur.get(seg)?;
        if i + 1 == len {
            return Some(v);
        }
        cur = v.as_object()?;
    }
    None
}

fn lift_wire_shape_to_logical_object(
    wire_obj: &IndexMap<String, Value>,
    variant: &InputVariantSchema,
) -> Result<Value, ()> {
    let mut logical = IndexMap::new();
    for f in &variant.fields {
        let v = if let Some(path) = f.wire_json_path.as_ref().filter(|p| !p.is_empty()) {
            get_value_at_json_path(wire_obj, path).ok_or(())?.clone()
        } else if let Some(ek) = f.wire_array_element_key.as_ref().filter(|s| !s.is_empty()) {
            let arr = wire_obj
                .get(f.name.as_str())
                .ok_or(())?
                .as_array()
                .ok_or(())?;
            let mut out: Vec<Value> = Vec::with_capacity(arr.len());
            for item in arr {
                let o = item.as_object().ok_or(())?;
                out.push(o.get(ek.as_str()).ok_or(())?.clone());
            }
            Value::Array(out)
        } else {
            match wire_obj.get(f.name.as_str()) {
                Some(val) => val.clone(),
                None => {
                    if f.required {
                        return Err(());
                    }
                    continue;
                }
            }
        };
        logical.insert(f.name.clone(), v);
    }
    Ok(Value::Object(logical))
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
    use crate::FieldType;
    match &f.wire {
        InputFieldWire::Inline(ty) => Ok((**ty).clone()),
        InputFieldWire::Registry(_) => {
            let nv = f.named_value(cgs).map_err(|_| ())?;
            match &nv.field_type {
                FieldType::Array => {
                    let spec = nv.array_items.as_ref().ok_or(())?;
                    Ok(InputType::Array {
                        element_type: Box::new(InputType::Value {
                            field_type: spec.field_type.clone(),
                            allowed_values: spec.allowed_values.clone(),
                        }),
                        min_length: None,
                        max_length: None,
                    })
                }
                FieldType::MultiSelect => Ok(InputType::Value {
                    field_type: FieldType::MultiSelect,
                    allowed_values: nv.allowed_values.clone(),
                }),
                _ => Ok(InputType::Value {
                    field_type: nv.field_type.clone(),
                    allowed_values: nv.allowed_values.clone(),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{
        InputFieldWire, InputType, NamedValueSchema, StringSemantics, ValueDomainKey,
    };
    use crate::FieldType;
    use crate::Value;
    use std::path::PathBuf;

    #[test]
    fn lifts_simple_object() {
        let mut cgs = CGS::new();
        cgs.values.insert(
            "typed_invoke_title".into(),
            NamedValueSchema {
                description: String::new(),
                field_type: FieldType::String,
                value_format: None,
                allowed_values: None,
                string_semantics: Some(StringSemantics::Short),
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
                role: None,
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
    fn proof_document_edit_v2_union_ctor_injects_discriminator() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !dir.exists() {
            return;
        }
        let cgs = crate::loader::load_schema_dir(&dir).expect("proof apis");
        let cap = cgs
            .capabilities
            .get("document_edit_v2")
            .expect("document_edit_v2");
        let InputType::Object { fields, .. } = &cap.input_schema.as_ref().expect("is").input_type
        else {
            panic!("object input");
        };
        let ops = fields
            .iter()
            .find(|f| f.name == "operations")
            .expect("operations");
        let InputFieldWire::Inline(ty) = &ops.wire else {
            panic!("inline");
        };
        let InputType::Array { element_type, .. } = ty.as_ref() else {
            panic!("operations array");
        };
        let arr_ty = InputType::Array {
            element_type: element_type.clone(),
            min_length: None,
            max_length: None,
        };
        let op_elem = Value::UnionCtor {
            ctor_label: "v101".into(),
            ctor_fields: {
                let mut m = IndexMap::new();
                m.insert("ref".into(), Value::String("blk".into()));
                m.insert("markdown".into(), Value::String("body".into()));
                m
            },
        };
        let v = Value::Array(vec![op_elem]);
        let p = InvokeInputPayload::lift(&v, &arr_ty, &cgs);
        let InvokeInputPayload::Typed(t) = p else {
            panic!("expected typed payload: {p:?}");
        };
        let out = t.to_value();
        let Value::Array(rows) = out else {
            panic!("array out: {out:?}");
        };
        let Value::Object(row) = rows.first().expect("one op") else {
            panic!("row: {:?}", rows.first());
        };
        assert_eq!(
            row.get("op").and_then(|x| x.as_str()),
            Some("replace_block")
        );
        assert_eq!(
            row.get("block")
                .and_then(|b| b.as_object())
                .and_then(|o| o.get("markdown"))
                .and_then(|x| x.as_str()),
            Some("body")
        );
    }

    #[test]
    fn proof_document_edit_v2_insert_before_wraps_block_markdown_elements() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apis/proof");
        if !dir.exists() {
            return;
        }
        let cgs = crate::loader::load_schema_dir(&dir).expect("proof apis");
        let cap = cgs
            .capabilities
            .get("document_edit_v2")
            .expect("document_edit_v2");
        let InputType::Object { fields, .. } = &cap.input_schema.as_ref().expect("is").input_type
        else {
            panic!("object input");
        };
        let ops = fields
            .iter()
            .find(|f| f.name == "operations")
            .expect("operations");
        let InputFieldWire::Inline(ty) = &ops.wire else {
            panic!("inline");
        };
        let InputType::Array { element_type, .. } = ty.as_ref() else {
            panic!("operations array");
        };
        let arr_ty = InputType::Array {
            element_type: element_type.clone(),
            min_length: None,
            max_length: None,
        };
        let op_elem = Value::UnionCtor {
            ctor_label: "v102".into(),
            ctor_fields: {
                let mut m = IndexMap::new();
                m.insert("ref".into(), Value::String("r1".into()));
                m.insert(
                    "blocks".into(),
                    Value::Array(vec![Value::String("a".into()), Value::String("b".into())]),
                );
                m
            },
        };
        let v = Value::Array(vec![op_elem]);
        let p = InvokeInputPayload::lift(&v, &arr_ty, &cgs);
        let InvokeInputPayload::Typed(t) = p else {
            panic!("expected typed payload: {p:?}");
        };
        let out = t.to_value();
        let Value::Array(rows) = out else {
            panic!("array out: {out:?}");
        };
        let Value::Object(row) = rows.first().expect("one op") else {
            panic!("row: {:?}", rows.first());
        };
        assert_eq!(
            row.get("op").and_then(|x| x.as_str()),
            Some("insert_before")
        );
        let blocks = row
            .get("blocks")
            .and_then(|x| x.as_array())
            .expect("blocks array");
        assert_eq!(blocks.len(), 2);
        assert_eq!(
            blocks[0]
                .as_object()
                .and_then(|o| o.get("markdown"))
                .and_then(|x| x.as_str()),
            Some("a")
        );
        assert_eq!(
            blocks[1]
                .as_object()
                .and_then(|o| o.get("markdown"))
                .and_then(|x| x.as_str()),
            Some("b")
        );
    }
}
