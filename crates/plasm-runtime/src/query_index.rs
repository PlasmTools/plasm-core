//! Exact observed query membership. Complete observations are never set-unioned.
use plasm_core::collection_codec::{
    CollectionCodec, CollectionFault, CollectionIdentity, Demand, RecordedCollection,
    RecordingCodec,
};
use plasm_core::{QueryExpr, Ref, CGS};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QueryCacheKey {
    entity: String,
    identity: CollectionIdentity,
}
impl QueryCacheKey {
    pub fn from_query(
        query: &QueryExpr,
        capability: &str,
        cgs: &CGS,
        environment: &plasm_compile::CmlEnv,
        epoch: u64,
    ) -> Result<Self, CollectionFault> {
        let environment: std::collections::BTreeMap<_, _> = environment.iter().collect();
        Ok(Self {
            entity: query.entity.to_string(),
            identity: CollectionIdentity::for_expression(
                cgs,
                &(query, capability, environment),
                epoch,
            )?,
        })
    }
    pub fn identity(&self) -> &CollectionIdentity {
        &self.identity
    }
}

/// Disagreement invalidates this observation epoch. Retaining the tombstone makes
/// branch merges associative: a third branch cannot resurrect disputed evidence.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryObservation {
    Observed(RecordedCollection<Ref>),
    Disputed,
}
#[derive(Debug, Clone, Default)]
pub struct QueryIndex {
    entries: HashMap<QueryCacheKey, QueryObservation>,
}
impl QueryIndex {
    pub fn get(&self, key: &QueryCacheKey) -> Option<&RecordedCollection<Ref>> {
        match self.entries.get(key)? {
            QueryObservation::Observed(record) => Some(record),
            QueryObservation::Disputed => None,
        }
    }
    pub fn insert(
        &mut self,
        key: QueryCacheKey,
        record: RecordedCollection<Ref>,
    ) -> Result<(), CollectionFault> {
        if record.identity() != key.identity() {
            return Err(CollectionFault::IdentityMismatch);
        }
        RecordingCodec::new().materialize(&record, Demand::Whole)?;
        self.merge_observation(key, QueryObservation::Observed(record));
        Ok(())
    }
    fn merge_observation(&mut self, key: QueryCacheKey, observation: QueryObservation) {
        use std::collections::hash_map::Entry;
        let same_expression = |other: &QueryCacheKey| {
            other.entity == key.entity
                && other.identity.catalog == key.identity.catalog
                && other.identity.expression == key.identity.expression
        };
        // Index residency is bounded to one observation epoch per expression.
        // Dropping an older cache entry never transfers its evidence to a newer one.
        if self
            .entries
            .keys()
            .any(|other| same_expression(other) && other.identity.epoch > key.identity.epoch)
        {
            return;
        }
        self.entries.retain(|other, _| {
            !same_expression(other) || other.identity.epoch >= key.identity.epoch
        });
        match self.entries.entry(key) {
            Entry::Vacant(slot) => {
                slot.insert(observation);
            }
            Entry::Occupied(mut slot) => {
                if slot.get() != &observation {
                    slot.insert(QueryObservation::Disputed);
                }
            }
        }
    }
    pub fn invalidate_entity_type(&mut self, entity: &str) {
        self.entries.retain(|key, _| key.entity != entity);
    }
    pub fn merge_from(&mut self, other: Self) {
        for (key, observation) in other.entries {
            self.merge_observation(key, observation);
        }
    }
    pub(crate) fn entries_snapshot(&self) -> HashMap<QueryCacheKey, QueryObservation> {
        self.entries.clone()
    }
    pub(crate) fn branch_write_keys(
        &self,
        base: &HashMap<QueryCacheKey, QueryObservation>,
    ) -> Vec<QueryCacheKey> {
        self.entries
            .iter()
            .filter(|(key, value)| base.get(*key) != Some(*value))
            .map(|(key, _)| key.clone())
            .collect()
    }
    pub(crate) fn detect_write_conflicts(
        session: &Self,
        branch: &Self,
        base: &HashMap<QueryCacheKey, QueryObservation>,
        write_set: &[QueryCacheKey],
    ) -> Vec<QueryCacheKey> {
        write_set
            .iter()
            .filter(|key| {
                crate::materialization_conflict::content_diverged(
                    base.get(*key),
                    branch.entries.get(*key),
                    session.entries.get(*key),
                )
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
impl QueryCacheKey {
    pub fn test(s: impl Into<String>) -> Self {
        let name = s.into();
        Self {
            entity: name.split('\0').next().unwrap().into(),
            identity: CollectionIdentity::for_expression(&CGS::new(), &name, 1).unwrap(),
        }
    }
}
#[cfg(test)]
impl QueryIndex {
    pub(crate) fn insert_test_observation(&mut self, key: QueryCacheKey, refs: Vec<Ref>) {
        let n = refs.len();
        let codec = RecordingCodec::new();
        let mut acquisition = codec.acquire(key.identity.clone());
        acquisition
            .push(
                &key.identity,
                0,
                refs.into(),
                n,
                plasm_core::collection_codec::PageTermination::Exhausted,
            )
            .unwrap();
        self.insert(key, acquisition.finish()).unwrap();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::collection_codec::{Observation, ResultCoverage};

    proptest::proptest! {
        #![proptest_config(proptest::test_runner::Config::with_cases(512))]
        #[test]
        fn observation_merge_is_associative_commutative_and_idempotent(
            a in proptest::collection::vec(0u8..5, 0..12),
            b in proptest::collection::vec(0u8..5, 0..12),
            c in proptest::collection::vec(0u8..5, 0..12),
            epochs in (0u64..3, 0u64..3, 0u64..3),
        ) {
            let key = QueryCacheKey::test("Item");
            let make = |values: &[u8], epoch: u64| {
                let mut key = key.clone();
                key.identity.epoch = epoch;
                let mut index = QueryIndex::default();
                index.insert_test_observation(key.clone(), values.iter().map(|v| Ref::new("Item", v.to_string())).collect());
                index
            };
            let aa = make(&a, epochs.0); let bb = make(&b, epochs.1); let cc = make(&c, epochs.2);
            let mut left = aa.clone(); left.merge_from(bb.clone()); left.merge_from(cc.clone());
            let mut bc = bb.clone(); bc.merge_from(cc);
            let mut right = aa.clone(); right.merge_from(bc);
            proptest::prop_assert_eq!(&left.entries, &right.entries);
            let mut ab = aa.clone(); ab.merge_from(bb.clone());
            let mut ba = bb; ba.merge_from(aa.clone());
            proptest::prop_assert_eq!(&ab.entries, &ba.entries);
            let mut same = aa.clone(); same.merge_from(aa.clone());
            proptest::prop_assert_eq!(&same.entries, &aa.entries);
            // Independent oracle: at the newest epoch only identical full
            // sequences can be reused, irrespective of branch merge order.
            let newest = epochs.0.max(epochs.1).max(epochs.2);
            let candidates: Vec<_> = [(epochs.0, &a), (epochs.1, &b), (epochs.2, &c)]
                .into_iter().filter(|(epoch, _)| *epoch == newest).map(|(_, rows)| rows).collect();
            let mut key = key;
            key.identity.epoch = newest;
            proptest::prop_assert_eq!(left.get(&key).is_some(), candidates.iter().all(|rows| *rows == candidates[0]));
            proptest::prop_assert_eq!(left.entries.len(), 1);
        }
    }

    #[test]
    fn query_scope_includes_catalog_expression_and_epoch() {
        let cgs = CGS::new();
        let q = QueryExpr::all("Item");
        let key = QueryCacheKey::from_query(&q, "query", &cgs, &Default::default(), 1).unwrap();
        assert_ne!(
            key,
            QueryCacheKey::from_query(&q, "query", &cgs, &Default::default(), 2).unwrap()
        );
        assert_ne!(
            key,
            QueryCacheKey::from_query(&q, "other", &cgs, &Default::default(), 1).unwrap()
        );
    }
    #[test]
    fn query_scope_binds_resolved_environment_without_insertion_order() {
        let cgs = CGS::new();
        let query = QueryExpr::all("Item");
        let environment: plasm_compile::CmlEnv = [
            ("scope".into(), plasm_core::Value::String("a".into())),
            ("receiver".into(), plasm_core::Value::Integer(7)),
        ]
        .into_iter()
        .collect();
        let reversed = environment
            .iter()
            .rev()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let original = QueryCacheKey::from_query(&query, "query", &cgs, &environment, 1).unwrap();
        assert_eq!(
            original,
            QueryCacheKey::from_query(&query, "query", &cgs, &reversed, 1).unwrap()
        );
        let mut other = environment;
        other.insert("scope".into(), plasm_core::Value::String("b".into()));
        assert_ne!(
            original,
            QueryCacheKey::from_query(&query, "query", &cgs, &other, 1).unwrap()
        );
    }

    #[test]
    fn index_preserves_empty_ordered_and_duplicate_membership() {
        for refs in [
            vec![],
            vec![
                Ref::new("Item", "b"),
                Ref::new("Item", "a"),
                Ref::new("Item", "b"),
            ],
        ] {
            let key = QueryCacheKey::test("Item");
            let mut index = QueryIndex::default();
            index.insert_test_observation(key.clone(), refs.clone());
            let record = index.get(&key).unwrap();
            assert_eq!(record.observed(), &refs);
            assert_eq!(record.coverage(), ResultCoverage::Complete);
            let snapshot = index.clone();
            if !refs.is_empty() {
                assert!(std::ptr::eq(
                    &record.observed()[0],
                    &snapshot.get(&key).unwrap().observed()[0]
                ));
            }
            index.invalidate_entity_type("Item");
            assert!(index.get(&key).is_none());
        }
    }
    #[test]
    fn index_releases_superseded_observation_epochs() {
        let mut index = QueryIndex::default();
        let mut key = QueryCacheKey::test("Item");
        for epoch in 1..32 {
            key.identity.epoch = epoch;
            index.insert_test_observation(key.clone(), vec![Ref::new("Item", epoch.to_string())]);
            assert_eq!(index.entries.len(), 1);
        }
        let latest = key.clone();
        key.identity.epoch = 1;
        index.insert_test_observation(key.clone(), vec![Ref::new("Item", "old")]);
        assert!(index.get(&key).is_none());
        assert!(index.get(&latest).is_some());
        assert_eq!(index.entries.len(), 1);
    }

    #[test]
    fn uncertain_or_wrong_scope_records_cannot_enter_index() {
        let key = QueryCacheKey::test("Item");
        let codec = RecordingCodec::<Ref>::new();
        let mut index = QueryIndex::default();
        let unknown = codec
            .record(key.identity.clone(), vec![], Observation::UnprovenPage)
            .unwrap();
        assert!(matches!(
            index.insert(key.clone(), unknown),
            Err(CollectionFault::Incomplete { .. })
        ));
        let wrong = codec
            .record(
                QueryCacheKey::test("Other").identity,
                vec![],
                Observation::Literal,
            )
            .unwrap();
        assert_eq!(
            index.insert(key.clone(), wrong),
            Err(CollectionFault::IdentityMismatch)
        );
        assert!(index.get(&key).is_none());
    }
    #[test]
    fn disputed_observations_never_reappear_on_later_merge() {
        let key = QueryCacheKey::test("Item");
        let mut a = QueryIndex::default();
        a.insert_test_observation(key.clone(), vec![Ref::new("Item", "a")]);
        let original = a.clone();
        let mut b = QueryIndex::default();
        b.insert_test_observation(key.clone(), vec![Ref::new("Item", "b")]);
        a.merge_from(b);
        a.merge_from(original);
        assert!(a.get(&key).is_none());
    }
}
