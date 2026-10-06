//! Streaming top-k with the same declared ordering and stable ties as row sort.
use crate::cache::CachedEntity;
use crate::row_predicate::{entity_field_path_value, BoundRowPredicate};
use plasm_core::{value_contract::ValueContract, value_order::Orderable, Value};
use std::{cmp::Ordering, collections::BinaryHeap, sync::Arc};

#[derive(Debug, Clone)]
pub struct TopKSpec {
    pub count: usize,
    pub sort_key: Vec<String>,
    pub descending: bool,
    pub row_filter: Vec<BoundRowPredicate>,
}
struct Entry {
    key: Value,
    entity: CachedEntity,
    sequence: usize,
    contract: Arc<ValueContract>,
    descending: bool,
}
impl Eq for Entry {}
impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        let order = if self.key.is_null() || other.key.is_null() {
            self.key.is_null().cmp(&other.key.is_null())
        } else {
            let order = self
                .contract
                .ordering()
                .expect("resolved ordering")
                .compare(&self.key, &other.key)
                .expect("validated common ordering domain");
            if self.descending {
                order.reverse()
            } else {
                order
            }
        };
        order.then_with(|| self.sequence.cmp(&other.sequence))
    }
}
pub struct TopKHeap {
    spec: TopKSpec,
    contract: Arc<ValueContract>,
    representative: Option<Value>,
    heap: BinaryHeap<Entry>,
    sequence: usize,
}
impl TopKHeap {
    pub fn new(spec: TopKSpec, contract: ValueContract) -> Result<Self, crate::RuntimeError> {
        contract.ordering()?;
        Ok(Self {
            spec,
            contract: Arc::new(contract),
            representative: None,
            heap: BinaryHeap::new(),
            sequence: 0,
        })
    }
    pub fn insert(&mut self, entity: CachedEntity) -> Result<(), crate::RuntimeError> {
        if !self.spec.row_filter.is_empty()
            && !crate::row_predicate::entity_matches_predicates(&entity, &self.spec.row_filter)?
        {
            return Ok(());
        }
        if let Some(field) = self.spec.sort_key.first() {
            crate::row_predicate::require_entity_field_available(&entity, field)?;
        }
        let key = entity_field_path_value(&entity, &self.spec.sort_key)
            .ok_or(crate::RuntimeError::TopKFieldUnobserved)?;
        let ordering = self.contract.ordering()?;
        ordering.validate(&key)?;
        if !key.is_null() {
            if let Some(prior) = &self.representative {
                ordering.compare(prior, &key)?;
            } else {
                self.representative = Some(key.clone());
            }
        }
        let sequence = self.sequence;
        self.sequence = sequence
            .checked_add(1)
            .ok_or(crate::RuntimeError::TopKSequenceOverflow)?;
        let entry = Entry {
            key,
            entity,
            sequence,
            contract: self.contract.clone(),
            descending: self.spec.descending,
        };
        if self.heap.len() < self.spec.count {
            self.heap.push(entry);
        } else if self.heap.peek().is_some_and(|worst| entry < *worst) {
            self.heap.pop();
            self.heap.push(entry);
        }
        Ok(())
    }
    pub fn into_sorted_entities(self) -> Vec<CachedEntity> {
        self.heap
            .into_sorted_vec()
            .into_iter()
            .map(|entry| entry.entity)
            .collect()
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
            unavailable_fields: Default::default(),
        }
    }

    #[test]
    fn top_k_money_is_exact_and_rejects_incomparable_values() {
        let mut heap = TopKHeap::new(
            TopKSpec {
                count: 1,
                sort_key: vec!["score".into()],
                descending: true,
                row_filter: vec![],
            },
            ValueContract::scalar(plasm_core::FieldType::Money),
        )
        .unwrap();
        let make = |name, amount: &str, currency: &str| {
            let mut row = entity(name, 0);
            row.fields.insert(
                "score".into(),
                TypedFieldValue::Money(plasm_core::MoneyValue::new(
                    amount.parse().unwrap(),
                    Some(currency.into()),
                )),
            );
            row
        };
        heap.insert(make("a", "9007199254740992", "USD")).unwrap();
        heap.insert(make("b", "9007199254740993", "USD")).unwrap();
        assert!(heap.insert(make("bad", "9007199254740994", "EUR")).is_err());
        assert_eq!(
            heap.into_sorted_entities()[0].reference.primary_slot_str(),
            "b"
        );
        let mut heap = TopKHeap::new(
            TopKSpec {
                count: 1,
                sort_key: vec!["score".into()],
                descending: false,
                row_filter: vec![],
            },
            ValueContract::scalar(plasm_core::FieldType::Integer),
        )
        .unwrap();
        let mut row = entity("invalid", 0);
        row.fields
            .insert("score".into(), TypedFieldValue::Object(IndexMap::new()));
        assert!(heap.insert(row).is_err());
    }

    #[test]
    fn top_k_descending_keeps_largest_scores() {
        let mut heap = TopKHeap::new(
            TopKSpec {
                count: 2,
                sort_key: vec!["score".into()],
                descending: true,
                row_filter: Vec::new(),
            },
            ValueContract::scalar(plasm_core::FieldType::Integer),
        )
        .unwrap();
        for (name, score) in [("a", 1), ("b", 3), ("c", 2)] {
            heap.insert(entity(name, score)).unwrap();
        }
        let names: Vec<_> = heap
            .into_sorted_entities()
            .into_iter()
            .map(|e| e.reference.primary_slot_str().to_string())
            .collect();
        assert_eq!(names, vec!["b".to_string(), "c".to_string()]);
    }
}

#[cfg(test)]
mod laws {
    use super::*;
    use plasm_core::{temporal_value::TemporalKind, Ref, TypedFieldValue};
    use proptest::prelude::*;
    fn row(id: usize, key: Value) -> CachedEntity {
        CachedEntity {
            reference: Ref::new("Item", id.to_string()),
            fields: [("key".into(), TypedFieldValue::from(key))].into(),
            relations: Default::default(),
            last_updated: 0,
            version: 0,
            completeness: crate::EntityCompleteness::Summary,
            unavailable_fields: Default::default(),
        }
    }
    proptest! {
        #[test]
        fn heap_matches_stable_full_sort_with_nulls_and_ties(values in proptest::collection::vec(proptest::option::of(-10i64..10),0..80), count in 0usize..90, descending in any::<bool>()) {
            let spec=TopKSpec { count,sort_key:vec!["key".into()],descending,row_filter:vec![] };
            let mut heap=TopKHeap::new(spec,ValueContract::scalar(plasm_core::FieldType::Integer)).unwrap();
            for (i,v) in values.iter().enumerate() { heap.insert(row(i,v.map(Value::Integer).unwrap_or(Value::Null))).unwrap(); }
            let mut expected:Vec<_>=values.iter().enumerate().collect();
            expected.sort_by(|(i,a),(j,b)| match (a,b) {
                (Some(a),Some(b)) => (if descending { b.cmp(a) } else { a.cmp(b) }).then(i.cmp(j)),
                _ => a.is_none().cmp(&b.is_none()).then(i.cmp(j)),
            });
            let expected:Vec<_>=expected.into_iter().take(count).map(|(i,_)|i.to_string()).collect();
            let actual:Vec<_>=heap.into_sorted_entities().iter().map(|r|r.reference.primary_slot_str().to_string()).collect();
            prop_assert_eq!(actual,expected);
        }
    }
    #[test]
    fn temporal_keys_follow_instants_and_reject_bad_singletons_and_empty_domains() {
        let spec = TopKSpec {
            count: 2,
            sort_key: vec!["key".into()],
            descending: false,
            row_filter: vec![],
        };
        assert!(TopKHeap::new(
            spec.clone(),
            ValueContract::scalar(plasm_core::FieldType::Json)
        )
        .is_err());
        let mut heap = TopKHeap::new(spec.clone(), TemporalKind::Datetime.contract()).unwrap();
        for (i, v) in [
            "2024-01-01T00:00:00Z",
            "2024-01-01T01:00:00+02:00",
            "2023-12-31T23:00:00Z",
        ]
        .iter()
        .enumerate()
        {
            heap.insert(row(i, Value::String((*v).into()))).unwrap();
        }
        let out = heap.into_sorted_entities();
        assert_eq!(
            out.iter()
                .map(|r| r.reference.primary_slot_str())
                .collect::<Vec<_>>(),
            ["1", "2"]
        );
        let mut heap = TopKHeap::new(spec, TemporalKind::Datetime.contract()).unwrap();
        assert!(heap
            .insert(row(0, Value::String("invalid".into())))
            .is_err());
        let mut missing = row(1, Value::Null);
        missing.fields.clear();
        assert!(heap.insert(missing).is_err());
    }
}
