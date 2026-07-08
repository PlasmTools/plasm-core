//! Typed literals for predicate comparisons and related IR (typed IR migration).
//!
//! [`TypedLiteral`] narrows [`crate::value::Value`] to shapes used after normalization in
//! predicates. [`TypedComparisonValue`] stores either a typed literal or a dynamic [`Value`]
//! for JSON/`entity_ref`/`PlasmInputRef` shapes that do not lift cleanly.

use crate::entity_ref_value::{EntityRefPayload, EntityRefValueError};
use crate::value::{PlasmInputRef, Value};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Strongly typed predicate RHS when it lifts from [`Value`].
#[derive(Debug, Clone, PartialEq)]
pub enum TypedLiteral {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(String),
    Array(Vec<TypedLiteral>),
    /// Normalized `entity_ref` compound or atomic constructor.
    EntityRef(EntityRefPayload),
    /// Compile-time plan hole (`__plasm_hole`).
    InputRef(PlasmInputRef),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypedLiteralError {
    UnsupportedPlasmHoleInCollection,
    /// Surface union constructors (`v101{…}`) are not predicate literals.
    UnionConstructor,
    /// Unquoted program phrase token — not a typed literal until lowered.
    PhraseIdent,
    EntityRef(EntityRefValueError),
}

impl TypedLiteral {
    /// Lossless projection to [`Value`].
    pub fn to_value(&self) -> Value {
        match self {
            TypedLiteral::Null => Value::Null,
            TypedLiteral::Bool(b) => Value::Bool(*b),
            TypedLiteral::Integer(i) => Value::Integer(*i),
            TypedLiteral::Float(f) => Value::Float(*f),
            TypedLiteral::String(s) => Value::String(s.clone()),
            TypedLiteral::Array(items) => Value::Array(items.iter().map(Self::to_value).collect()),
            TypedLiteral::EntityRef(p) => p.to_value(),
            TypedLiteral::InputRef(r) => Value::PlasmInputRef(r.clone()),
        }
    }

    /// Lift from [`Value`] when the shape is a legal typed literal or entity_ref constructor.
    pub fn try_from_value(v: &Value) -> Result<Self, TypedLiteralError> {
        match v {
            Value::UnionCtor { .. } => Err(TypedLiteralError::UnionConstructor),
            Value::PlasmInputRef(r) => Ok(TypedLiteral::InputRef(r.clone())),
            Value::Null => Ok(TypedLiteral::Null),
            Value::Bool(b) => Ok(TypedLiteral::Bool(*b)),
            Value::Integer(i) => Ok(TypedLiteral::Integer(*i)),
            Value::Float(f) => Ok(TypedLiteral::Float(*f)),
            Value::String(s) => Ok(TypedLiteral::String(s.clone())),
            Value::PhraseIdent(_) => Err(TypedLiteralError::PhraseIdent),
            Value::Array(arr) => {
                let mut out = Vec::with_capacity(arr.len());
                for item in arr {
                    out.push(Self::try_from_value(item)?);
                }
                Ok(TypedLiteral::Array(out))
            }
            Value::Object(_) => match EntityRefPayload::try_from_value(v) {
                Ok(p) => Ok(TypedLiteral::EntityRef(p)),
                Err(e) => Err(TypedLiteralError::EntityRef(e)),
            },
        }
    }
}

impl TryFrom<Value> for TypedLiteral {
    type Error = TypedLiteralError;

    fn try_from(value: Value) -> Result<Self, Self::Error> {
        Self::try_from_value(&value)
    }
}

impl From<TypedLiteral> for Value {
    fn from(t: TypedLiteral) -> Self {
        t.to_value()
    }
}

/// Predicate comparison payload: typed literal when liftable, otherwise preserves dynamic [`Value`].
#[derive(Debug, Clone, PartialEq)]
pub struct TypedComparisonValue {
    inner: ComparisonInner,
}

#[derive(Debug, Clone, PartialEq)]
enum ComparisonInner {
    Typed(TypedLiteral),
    Dynamic(Value),
}

impl TypedComparisonValue {
    #[must_use]
    pub fn from_value(v: Value) -> Self {
        match TypedLiteral::try_from_value(&v) {
            Ok(t) => Self {
                inner: ComparisonInner::Typed(t),
            },
            Err(_) => Self {
                inner: ComparisonInner::Dynamic(v),
            },
        }
    }

    #[must_use]
    pub fn typed_literal(&self) -> Option<&TypedLiteral> {
        match &self.inner {
            ComparisonInner::Typed(t) => Some(t),
            ComparisonInner::Dynamic(_) => None,
        }
    }

    #[must_use]
    pub fn to_value(&self) -> Value {
        match &self.inner {
            ComparisonInner::Typed(t) => t.to_value(),
            ComparisonInner::Dynamic(v) => v.clone(),
        }
    }

    #[must_use]
    pub fn is_dynamic(&self) -> bool {
        matches!(self.inner, ComparisonInner::Dynamic(_))
    }
}

impl Serialize for TypedComparisonValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TypedComparisonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Value::deserialize(deserializer)?;
        Ok(Self::from_value(v))
    }
}

impl From<Value> for TypedComparisonValue {
    fn from(value: Value) -> Self {
        Self::from_value(value)
    }
}

impl From<TypedLiteral> for TypedComparisonValue {
    fn from(value: TypedLiteral) -> Self {
        Self {
            inner: ComparisonInner::Typed(value),
        }
    }
}

macro_rules! impl_from_primitive {
    ($($t:ty => $variant:ident),* $(,)?) => {
        $(
            impl From<$t> for TypedComparisonValue {
                fn from(v: $t) -> Self {
                    TypedLiteral::$variant(v.into()).into()
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

impl From<&str> for TypedComparisonValue {
    fn from(s: &str) -> Self {
        TypedLiteral::String(s.to_string()).into()
    }
}

impl From<String> for TypedComparisonValue {
    fn from(s: String) -> Self {
        TypedLiteral::String(s).into()
    }
}

impl From<i32> for TypedComparisonValue {
    fn from(i: i32) -> Self {
        TypedLiteral::Integer(i64::from(i)).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    #[test]
    fn typed_literal_roundtrips_scalar() {
        for v in [
            Value::Null,
            Value::Bool(true),
            Value::Integer(-3),
            Value::Float(1.5),
            Value::String("x".into()),
        ] {
            let t = TypedLiteral::try_from_value(&v).expect("scalar");
            assert_eq!(t.to_value(), v);
        }
    }

    #[test]
    fn comparison_payload_serializes_like_value() {
        let tc = TypedComparisonValue::from_value(Value::String("a".into()));
        let json = serde_json::to_string(&tc).unwrap();
        assert_eq!(json, "\"a\"");
        let back: TypedComparisonValue = serde_json::from_str(&json).unwrap();
        assert_eq!(back.to_value(), Value::String("a".into()));
    }

    #[test]
    fn dynamic_preserves_non_liftable_object() {
        // Scalar-only compound maps lift to `TypedLiteral::EntityRef`; nested null breaks
        // `EntityRefPayload` parsing so the payload stays dynamic.
        let mut m = IndexMap::new();
        m.insert("k".into(), Value::Null);
        let v = Value::Object(m);
        let tc = TypedComparisonValue::from_value(v.clone());
        assert!(tc.is_dynamic());
        assert_eq!(tc.to_value(), v);
    }
}
