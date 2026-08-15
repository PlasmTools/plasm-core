//! Typed row field storage — algebraic [`TypedFieldValue`] for decoded entity maps (typed IR migration).
//!
//! Values serialize exactly like [`crate::value::Value`] JSON. Without CGS per field, [`TypedFieldValue::from`]
//! lifts wire [`Value`] structurally (nested maps and arrays). With [`FieldType`], [`TypedFieldValue::from_value_in_field`]
//! preserves opaque JSON blobs and normalized [`crate::entity_ref_value::EntityRefPayload`] for [`FieldType::EntityRef`].

use crate::entity_ref_value::EntityRefPayload;
use crate::value::{PlasmInputRef, Value};
use crate::FieldType;
use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// One decoded entity field: algebraic JSON (+ normalized [`EntityRefPayload`] / opaque JSON when schema-guided).
#[derive(Debug, Clone, PartialEq)]
pub enum TypedFieldValue {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Array(Vec<TypedFieldValue>),
    Object(IndexMap<String, TypedFieldValue>),
    /// Normalized `entity_ref` payload when [`FieldType::EntityRef`] applies and the wire shape parses.
    EntityRef(EntityRefPayload),
    PlasmInputRef(PlasmInputRef),
    /// Arbitrary subtree (`FieldType::Json`, `Blob`, attachment blobs) stored verbatim.
    Json(Value),
}

impl TypedFieldValue {
    /// Structural lift from [`Value`] (no CGS): nested JSON maps become [`TypedFieldValue::Object`].
    #[must_use]
    pub fn from_value(v: Value) -> Self {
        Self::from(v)
    }

    /// Lift using CGS [`FieldType`] so opaque JSON / entity refs are tagged distinctly when applicable.
    #[must_use]
    pub fn from_value_in_field(field_type: &FieldType, v: Value) -> Self {
        match field_type {
            FieldType::Json | FieldType::Blob => Self::Json(v),
            FieldType::EntityRef { .. } => match EntityRefPayload::try_from_value(&v) {
                Ok(p) => Self::EntityRef(p),
                Err(_) => Self::from(v),
            },
            _ => Self::from(v),
        }
    }

    /// Lossless projection to [`Value`] for predicates, CML, HTTP, and serde snapshots.
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            TypedFieldValue::Null => Value::Null,
            TypedFieldValue::Bool(b) => Value::Bool(*b),
            TypedFieldValue::Integer(i) => Value::Integer(*i),
            TypedFieldValue::Float(f) => Value::Float(*f),
            TypedFieldValue::String(s) => Value::String(s.clone()),
            TypedFieldValue::Array(a) => Value::Array(a.iter().map(Self::to_value).collect()),
            TypedFieldValue::Object(m) => {
                Value::Object(m.iter().map(|(k, v)| (k.clone(), v.to_value())).collect())
            }
            TypedFieldValue::EntityRef(p) => p.to_value(),
            TypedFieldValue::PlasmInputRef(r) => Value::PlasmInputRef(r.clone()),
            TypedFieldValue::Json(v) => v.clone(),
        }
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, TypedFieldValue::Null)
            || matches!(self, TypedFieldValue::Json(v) if matches!(v, Value::Null))
    }

    #[must_use]
    pub fn into_value(self) -> Value {
        self.to_value()
    }
}

impl Serialize for TypedFieldValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TypedFieldValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::from(Value::deserialize(deserializer)?))
    }
}

impl From<Value> for TypedFieldValue {
    fn from(v: Value) -> Self {
        match v {
            Value::UnionCtor { .. } => TypedFieldValue::Json(v),
            Value::PlasmInputRef(r) => TypedFieldValue::PlasmInputRef(r),
            Value::Null => TypedFieldValue::Null,
            Value::Bool(b) => TypedFieldValue::Bool(b),
            Value::Integer(i) => TypedFieldValue::Integer(i),
            Value::Float(f) => TypedFieldValue::Float(f),
            Value::String(s) | Value::PhraseIdent(s) => TypedFieldValue::String(s),
            Value::Array(a) => TypedFieldValue::Array(a.into_iter().map(Self::from).collect()),
            Value::Money(_) => TypedFieldValue::Json(v),
            Value::Object(m) => {
                TypedFieldValue::Object(m.into_iter().map(|(k, v)| (k, Self::from(v))).collect())
            }
        }
    }
}

impl From<TypedFieldValue> for Value {
    fn from(tf: TypedFieldValue) -> Self {
        tf.to_value()
    }
}

macro_rules! impl_from_primitive {
    ($($t:ty => $variant:ident),* $(,)?) => {
        $(
            impl From<$t> for TypedFieldValue {
                fn from(v: $t) -> Self {
                    TypedFieldValue::$variant(v.into())
                }
            }
        )*
    };
}

impl_from_primitive! {
    bool => Bool,
    i64 => Integer,
    f64 => Float,
}

impl From<i32> for TypedFieldValue {
    fn from(v: i32) -> Self {
        TypedFieldValue::Integer(i64::from(v))
    }
}

impl From<String> for TypedFieldValue {
    fn from(s: String) -> Self {
        TypedFieldValue::String(s)
    }
}

impl From<&str> for TypedFieldValue {
    fn from(s: &str) -> Self {
        TypedFieldValue::String(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    #[test]
    fn structural_roundtrips_json_shapes() {
        let v = Value::Object({
            let mut m = IndexMap::new();
            m.insert("a".into(), Value::Integer(1));
            m.insert(
                "b".into(),
                Value::Array(vec![Value::String("x".into()), Value::Bool(true)]),
            );
            m
        });
        let tf = TypedFieldValue::from(v.clone());
        assert_eq!(tf.to_value(), v);
    }

    #[test]
    fn json_field_preserves_opaque_object() {
        let v = Value::Object({
            let mut m = IndexMap::new();
            m.insert("k".into(), Value::Null);
            m
        });
        let tf = TypedFieldValue::from_value_in_field(&FieldType::Json, v.clone());
        match tf {
            TypedFieldValue::Json(inner) => assert_eq!(inner, v),
            other => panic!("expected Json variant: {other:?}"),
        }
    }
}
