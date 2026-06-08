//! Streaming top-k over paginated entity pages (sort+limit pushdown).

use crate::cache::CachedEntity;
use crate::row_predicate::{entity_field_path_value, JsonRowPredicate};
use std::cmp::Ordering;
use std::collections::BinaryHeap;

#[derive(Debug, Clone)]
pub struct TopKSpec {
    pub count: usize,
    pub sort_key: Vec<String>,
    pub descending: bool,
    pub row_filter: Vec<JsonRowPredicate>,
}

#[derive(Debug)]
struct TopKEntry {
    sort_key: serde_json::Value,
    entity: CachedEntity,
}

/// Min-heap ordering for descending top-k (keep the k largest keys).
struct MinEntry(TopKEntry);

impl Eq for MinEntry {}

impl PartialEq for MinEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl PartialOrd for MinEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MinEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_json_values(&self.0.sort_key, &other.0.sort_key).reverse()
    }
}

/// Max-heap ordering for ascending top-k (keep the k smallest keys).
struct MaxEntry(TopKEntry);

impl Eq for MaxEntry {}

impl PartialEq for MaxEntry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl PartialOrd for MaxEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MaxEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_json_values(&self.0.sort_key, &other.0.sort_key)
    }
}

pub struct TopKHeap {
    spec: TopKSpec,
    min_heap: BinaryHeap<MinEntry>,
    max_heap: BinaryHeap<MaxEntry>,
}

impl TopKHeap {
    pub fn new(spec: TopKSpec) -> Self {
        Self {
            spec,
            min_heap: BinaryHeap::new(),
            max_heap: BinaryHeap::new(),
        }
    }

    pub fn insert(&mut self, entity: CachedEntity) {
        if !self.spec.row_filter.is_empty()
            && !crate::row_predicate::entity_matches_predicates(&entity, &self.spec.row_filter)
        {
            return;
        }
        let sort_key = entity_field_path_value(&entity, &self.spec.sort_key).unwrap_or_default();
        let entry = TopKEntry { sort_key, entity };
        if self.spec.descending {
            if self.min_heap.len() < self.spec.count {
                self.min_heap.push(MinEntry(entry));
                return;
            }
            if let Some(worst) = self.min_heap.peek() {
                if cmp_json_values(&entry.sort_key, &worst.0.sort_key) == Ordering::Greater {
                    self.min_heap.pop();
                    self.min_heap.push(MinEntry(entry));
                }
            }
        } else if self.max_heap.len() < self.spec.count {
            self.max_heap.push(MaxEntry(entry));
        } else if let Some(worst) = self.max_heap.peek() {
            if cmp_json_values(&entry.sort_key, &worst.0.sort_key) == Ordering::Less {
                self.max_heap.pop();
                self.max_heap.push(MaxEntry(entry));
            }
        }
    }

    pub fn into_sorted_entities(self) -> Vec<CachedEntity> {
        let mut entries: Vec<CachedEntity> = if self.spec.descending {
            self.min_heap.into_iter().map(|e| e.0.entity).collect()
        } else {
            self.max_heap.into_iter().map(|e| e.0.entity).collect()
        };
        entries.sort_by(|a, b| {
            let av = entity_field_path_value(a, &self.spec.sort_key).unwrap_or_default();
            let bv = entity_field_path_value(b, &self.spec.sort_key).unwrap_or_default();
            let ord = cmp_json_values(&av, &bv);
            if self.spec.descending {
                ord.reverse()
            } else {
                ord
            }
        });
        entries
    }
}

fn cmp_json_values(a: &serde_json::Value, b: &serde_json::Value) -> Ordering {
    match (a, b) {
        (serde_json::Value::Null, serde_json::Value::Null) => Ordering::Equal,
        (serde_json::Value::Null, _) => Ordering::Less,
        (_, serde_json::Value::Null) => Ordering::Greater,
        (serde_json::Value::Number(x), serde_json::Value::Number(y)) => x
            .as_f64()
            .unwrap_or(f64::NAN)
            .partial_cmp(&y.as_f64().unwrap_or(f64::NAN))
            .unwrap_or(Ordering::Equal),
        (serde_json::Value::String(x), serde_json::Value::String(y)) => x.cmp(y),
        (serde_json::Value::Bool(x), serde_json::Value::Bool(y)) => x.cmp(y),
        _ => a.to_string().cmp(&b.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EntityCompleteness;
    use indexmap::IndexMap;
    use plasm_core::{Ref, TypedFieldValue, Value};

    fn entity(name: &str, score: i64) -> CachedEntity {
        let mut fields = IndexMap::new();
        fields.insert(
            "name".to_string(),
            TypedFieldValue::from(Value::String(name.to_string())),
        );
        fields.insert(
            "score".to_string(),
            TypedFieldValue::from(Value::Integer(score)),
        );
        CachedEntity {
            reference: Ref::new("Berry", name),
            fields,
            relations: IndexMap::new(),
            last_updated: 0,
            version: 0,
            completeness: EntityCompleteness::Summary,
        }
    }

    #[test]
    fn top_k_descending_keeps_largest_scores() {
        let mut heap = TopKHeap::new(TopKSpec {
            count: 2,
            sort_key: vec!["score".into()],
            descending: true,
            row_filter: Vec::new(),
        });
        for (name, score) in [("a", 1), ("b", 3), ("c", 2)] {
            heap.insert(entity(name, score));
        }
        let names: Vec<_> = heap
            .into_sorted_entities()
            .into_iter()
            .map(|e| e.reference.primary_slot_str().to_string())
            .collect();
        assert_eq!(names, vec!["b".to_string(), "c".to_string()]);
    }
}
