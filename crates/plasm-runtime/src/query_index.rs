//! Exact scoped query materialization index (equality predicates on scope params).

use plasm_core::{CompOp, Predicate, QueryExpr, Ref};
use std::collections::HashMap;

/// Stable key for a fully-scoped list/query (equality on all bound scope parameters).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueryCacheKey(String);

impl QueryCacheKey {
    pub fn from_query(query: &QueryExpr, capability_name: &str) -> Option<Self> {
        let pred = query.predicate.as_ref()?;
        let mut pairs = equality_pairs(pred)?;
        if pairs.is_empty() {
            return None;
        }
        let mut parts = vec![
            query.entity.as_str().to_string(),
            capability_name.to_string(),
        ];
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in pairs {
            parts.push(format!("{k}={v}"));
        }
        Some(Self(parts.join("\0")))
    }
}

fn equality_pairs(pred: &Predicate) -> Option<Vec<(String, String)>> {
    match pred {
        Predicate::Comparison {
            field,
            op: CompOp::Eq,
            value,
        } => Some(vec![(field.clone(), comparison_value_key(value))]),
        Predicate::And { args } => {
            let mut out = Vec::new();
            for child in args {
                out.extend(equality_pairs(child)?);
            }
            Some(out)
        }
        _ => None,
    }
}

fn comparison_value_key(value: &plasm_core::TypedComparisonValue) -> String {
    let v = value.to_value();
    match &v {
        plasm_core::Value::String(s) => s.clone(),
        plasm_core::Value::Integer(i) => i.to_string(),
        plasm_core::Value::Bool(b) => b.to_string(),
        plasm_core::Value::Float(f) => f.to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| format!("{other:?}")),
    }
}

#[derive(Debug, Clone, Default)]
pub struct QueryIndex {
    entries: HashMap<QueryCacheKey, Vec<Ref>>,
}

impl QueryIndex {
    pub fn get(&self, key: &QueryCacheKey) -> Option<&[Ref]> {
        self.entries.get(key).map(|v| v.as_slice())
    }

    pub fn insert(&mut self, key: QueryCacheKey, refs: Vec<Ref>) {
        if refs.is_empty() {
            self.entries.remove(&key);
        } else {
            self.entries.insert(key, refs);
        }
    }

    pub fn invalidate_entity_type(&mut self, entity_type: &str) {
        self.entries.retain(|_, refs| {
            refs.first()
                .is_none_or(|r| r.entity_type.as_str() != entity_type)
        });
    }

    pub fn merge_from(&mut self, other: QueryIndex) {
        self.entries.extend(other.entries);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{EntityName, Value};

    #[test]
    fn query_cache_key_from_scoped_predicate() {
        let mut q = QueryExpr::filtered(EntityName::new("Label"), Predicate::eq("n", 1));
        q.capability_name = Some(plasm_core::CapabilityName::new("issue_label_query"));
        let key = QueryCacheKey::from_query(&q, "issue_label_query").expect("key");
        assert!(key.0.contains("issue_label_query"));
    }

    #[test]
    fn query_index_roundtrip() {
        let mut idx = QueryIndex::default();
        let key = QueryCacheKey("test".to_string());
        let r = Ref::new("Label", "1");
        idx.insert(key.clone(), vec![r.clone()]);
        assert_eq!(idx.get(&key), Some([r].as_slice()));
    }
}
