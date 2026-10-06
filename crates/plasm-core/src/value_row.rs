//! Native computation records. A row is always an object; absence is not null.
use crate::Value;
use indexmap::IndexMap;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ValueRowError {
    #[error("row requires a record value")]
    ExpectedRecord,
}
use std::ops::{Deref, Index};

#[derive(Debug, Clone, PartialEq)]
pub struct ValueRow(Value);

impl ValueRow {
    /// Normalize a scalar compute result into its explicit value column.
    pub fn from_output(value: Value) -> Self {
        match value {
            Value::Object(_) => Self(value),
            other => [("value".into(), other)].into_iter().collect(),
        }
    }
    pub fn new() -> Self {
        Self(Value::Object(IndexMap::new()))
    }
    pub fn fields(&self) -> &IndexMap<String, Value> {
        match &self.0 {
            Value::Object(fields) => fields,
            _ => unreachable!("record invariant"),
        }
    }
    pub fn fields_mut(&mut self) -> &mut IndexMap<String, Value> {
        match &mut self.0 {
            Value::Object(fields) => fields,
            _ => unreachable!("record invariant"),
        }
    }
    pub fn into_fields(self) -> IndexMap<String, Value> {
        match self.0 {
            Value::Object(fields) => fields,
            _ => unreachable!("record invariant"),
        }
    }
    pub fn into_value(self) -> Value {
        self.0
    }
    pub fn insert(&mut self, key: String, value: Value) -> Option<Value> {
        self.fields_mut().insert(key, value)
    }
    pub fn shift_remove(&mut self, key: &str) -> Option<Value> {
        self.fields_mut().shift_remove(key)
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.fields().contains_key(key)
    }
    pub fn keys(&self) -> indexmap::map::Keys<'_, String, Value> {
        self.fields().keys()
    }
    pub fn iter(&self) -> indexmap::map::Iter<'_, String, Value> {
        self.fields().iter()
    }
    pub fn len(&self) -> usize {
        self.fields().len()
    }
    pub fn is_empty(&self) -> bool {
        self.fields().is_empty()
    }
}
impl Default for ValueRow {
    fn default() -> Self {
        Self::new()
    }
}
impl Deref for ValueRow {
    type Target = Value;
    fn deref(&self) -> &Value {
        &self.0
    }
}
impl From<IndexMap<String, Value>> for ValueRow {
    fn from(fields: IndexMap<String, Value>) -> Self {
        Self(Value::Object(fields))
    }
}
impl From<ValueRow> for IndexMap<String, Value> {
    fn from(row: ValueRow) -> Self {
        row.into_fields()
    }
}
impl TryFrom<Value> for ValueRow {
    type Error = ValueRowError;
    fn try_from(value: Value) -> Result<Self, ValueRowError> {
        match value {
            Value::Object(_) => Ok(Self(value)),
            _ => Err(ValueRowError::ExpectedRecord),
        }
    }
}
impl FromIterator<(String, Value)> for ValueRow {
    fn from_iter<T: IntoIterator<Item = (String, Value)>>(iter: T) -> Self {
        Self::from(iter.into_iter().collect::<IndexMap<_, _>>())
    }
}
impl IntoIterator for ValueRow {
    type Item = (String, Value);
    type IntoIter = indexmap::map::IntoIter<String, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.into_fields().into_iter()
    }
}
impl<'a> IntoIterator for &'a ValueRow {
    type Item = (&'a String, &'a Value);
    type IntoIter = indexmap::map::Iter<'a, String, Value>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl Index<&str> for ValueRow {
    type Output = Value;
    fn index(&self, key: &str) -> &Value {
        &self.fields()[key]
    }
}

impl serde::Serialize for ValueRow {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl<'de> serde::Deserialize<'de> for ValueRow {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        <IndexMap<String, Value> as serde::Deserialize>::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn row_boundary_roundtrip_preserves_native_nested_types_and_presence() {
        let row = ValueRow::from_iter([
            ("integer".into(), Value::Integer(i64::MIN)),
            ("unsigned".into(), Value::Unsigned(u64::MAX)),
            ("null".into(), Value::Null),
            (
                "nested".into(),
                Value::Array(vec![
                    Value::Money(crate::MoneyValue::new(
                        "1.0000000000000000001".parse().unwrap(),
                        Some("USD".into()),
                    )),
                    Value::String("[1]".into()),
                    Value::Bool(true),
                ]),
            ),
        ]);
        let wire = serde_json::to_vec(&row).unwrap();
        let restored: ValueRow = serde_json::from_slice(&wire).unwrap();
        assert_eq!(restored, row);
        assert!(!restored.contains_key("absent"));
        assert!(matches!(
            &restored["nested"].as_array().unwrap()[0],
            Value::Money(_)
        ));
        assert!(serde_json::from_str::<ValueRow>("[1]").is_err());
        assert!(ValueRow::try_from(Value::String("scalar".into())).is_err());
    }
}
