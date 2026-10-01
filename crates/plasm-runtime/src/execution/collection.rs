//! Logical membership and physical residency are separate, immutable observations.
use crate::CachedEntity;
use plasm_core::collection_codec::{
    CollectionCodec, CollectionFault, CollectionIdentity, Demand, Observation, RecordedCollection,
    RecordingCodec, ResultCoverage, SharedRows, Transform,
};
use plasm_core::Ref;

#[derive(Debug, Clone)]
pub enum PayloadResidency {
    Materialized(SharedRows<CachedEntity>),
    Graph,
}

#[derive(Debug, Clone)]
pub struct ExecutionCollection {
    membership: RecordedCollection<Ref>,
    entities: SharedRows<CachedEntity>,
    graph_backed: bool,
}

impl ExecutionCollection {
    pub fn observe_for(
        cgs: &plasm_core::CGS,
        expression: &impl serde::Serialize,
        epoch: u64,
        entities: Vec<CachedEntity>,
        observation: Observation,
    ) -> Result<Self, CollectionFault> {
        Self::observe(
            CollectionIdentity::for_expression(cgs, expression, epoch)?,
            entities,
            observation,
        )
    }
    pub fn observe(
        identity: CollectionIdentity,
        entities: Vec<CachedEntity>,
        observation: Observation,
    ) -> Result<Self, CollectionFault> {
        let references = entities.iter().map(|row| row.reference.clone()).collect();
        let record = RecordingCodec::new().record(identity, references, observation)?;
        Self::materialized(record, entities.into())
    }

    pub fn materialized(
        membership: RecordedCollection<Ref>,
        entities: SharedRows<CachedEntity>,
    ) -> Result<Self, CollectionFault> {
        let codec = RecordingCodec::new();
        let prefix = codec.validate_prefix(
            &mut membership.observed().iter(),
            &mut entities.iter().map(|entity| &entity.reference),
        )?;
        if prefix.discarded() != 0 {
            return Err(CollectionFault::Conservation);
        }
        Ok(Self {
            membership,
            entities,
            graph_backed: false,
        })
    }

    pub fn graph(membership: RecordedCollection<Ref>) -> Self {
        Self {
            membership,
            entities: Vec::new().into(),
            graph_backed: true,
        }
    }
    pub fn membership(&self) -> &RecordedCollection<Ref> {
        &self.membership
    }
    pub fn is_graph_backed(&self) -> bool {
        self.graph_backed
    }
    /// Physical hot rows; logical count comes only from recorded membership.
    pub fn resident_entities(&self) -> &SharedRows<CachedEntity> {
        &self.entities
    }
    pub fn count(&self) -> usize {
        self.membership.observed().len()
    }
    pub fn coverage(&self) -> ResultCoverage {
        self.membership.coverage()
    }
    pub fn materialize(
        &self,
        demand: Demand,
    ) -> Result<&SharedRows<CachedEntity>, CollectionFault> {
        RecordingCodec::new().materialize(&self.membership, demand)?;
        if self.graph_backed && self.count() != 0 {
            return Err(CollectionFault::NotResident);
        }
        Ok(&self.entities)
    }
    pub fn with_materialization(
        &self,
        entities: SharedRows<CachedEntity>,
    ) -> Result<Self, CollectionFault> {
        Self::materialized(self.membership.clone(), entities)
    }
    /// Record a completed operator with its actual ordered dependency ports.
    pub fn evaluate(
        identity: CollectionIdentity,
        inputs: &[&Self],
        entities: SharedRows<CachedEntity>,
    ) -> Result<Self, CollectionFault> {
        let rows = entities.iter().map(|row| row.reference.clone()).collect();
        if inputs.is_empty() {
            let membership = RecordingCodec::new().record(identity, rows, Observation::Literal)?;
            return Self::materialized(membership, entities);
        }
        Self::derive(
            identity,
            inputs,
            Transform::Evaluate { rows },
            PayloadResidency::Materialized(entities),
        )
    }

    /// Filter records source positions, preserving payload allocation and order.
    pub fn filter(
        &self,
        expression: &impl serde::Serialize,
        retained: &[usize],
        captures: &[&Self],
    ) -> Result<Self, CollectionFault> {
        let mut inputs = vec![self];
        inputs.extend_from_slice(captures);
        let capture_ids: Vec<_> = captures
            .iter()
            .map(|input| input.membership.identity().clone())
            .collect();
        let identity = self.membership.identity().derived(expression)?;
        let payloads = if self.graph_backed {
            PayloadResidency::Graph
        } else {
            PayloadResidency::Materialized(self.entities.select(retained.iter().copied())?)
        };
        Self::derive(
            identity,
            &inputs,
            Transform::Filter {
                retained,
                captures: &capture_ids,
            },
            payloads,
        )
    }

    /// A delivery window retains the full logical proof. It cannot satisfy a
    /// physical materialization demand until all occurrences are resident.
    pub fn delivery(&self, range: std::ops::Range<usize>) -> Result<Self, CollectionFault> {
        self.materialize(Demand::Observed)?;
        let entities = self.entities.select(range)?;
        Ok(Self {
            graph_backed: entities.len() != self.count(),
            entities,
            membership: self.membership.clone(),
        })
    }

    /// Bind one already observed occurrence as an exact singleton port.
    /// The enclosing map retains the evidence of the parent collection.
    pub fn capture(&self, index: usize) -> Result<Self, CollectionFault> {
        self.materialize(Demand::Observed)?;
        let reference = self
            .membership
            .observed()
            .get(index)
            .ok_or(CollectionFault::Conservation)?
            .clone();
        let record = RecordingCodec::new().record(
            self.membership.identity().derived(&("capture", index))?,
            vec![reference],
            Observation::ExactOutput { decoded: 1 },
        )?;
        Self::materialized(record, self.entities.select([index])?)
    }

    /// Projection changes fields, never membership or relation evidence.
    pub fn map_fields(
        &self,
        mut project: impl FnMut(&mut indexmap::IndexMap<String, plasm_core::TypedFieldValue>),
    ) -> Self {
        let entities = self
            .entities
            .iter()
            .map(|observed| {
                let mut row = observed.clone();
                project(&mut row.fields);
                row
            })
            .collect::<Vec<_>>()
            .into();
        Self {
            membership: self.membership.clone(),
            entities,
            graph_backed: self.graph_backed,
        }
    }

    pub fn flat_map(
        &self,
        expression: &impl serde::Serialize,
        children: &[Self],
    ) -> Result<Self, CollectionFault> {
        let mut inputs = vec![self];
        inputs.extend(children.iter());
        let identities: Vec<_> = children
            .iter()
            .map(|c| c.membership.identity().clone())
            .collect();
        let payloads = if children.iter().any(Self::is_graph_backed) {
            PayloadResidency::Graph
        } else {
            PayloadResidency::Materialized(SharedRows::concat(
                children.iter().map(Self::resident_entities),
            ))
        };
        Self::derive(
            self.membership.identity().derived(expression)?,
            &inputs,
            Transform::FlatMap {
                children: &identities,
            },
            payloads,
        )
    }

    /// Replace one field observation without changing membership or copying siblings.
    pub fn replace(&self, index: usize, row: CachedEntity) -> Result<Self, CollectionFault> {
        if self.entities.get(index).map(|r| &r.reference) != Some(&row.reference) {
            return Err(CollectionFault::Conservation);
        }
        let before = self.entities.select(0..index)?;
        let changed = SharedRows::from(vec![row]);
        let after = self.entities.select(index + 1..self.entities.len())?;
        Self::materialized(
            self.membership.clone(),
            SharedRows::concat([&before, &changed, &after]),
        )
    }

    pub fn derive(
        identity: CollectionIdentity,
        inputs: &[&Self],
        operation: Transform<'_, Ref>,
        payloads: PayloadResidency,
    ) -> Result<Self, CollectionFault> {
        let records: Vec<_> = inputs.iter().map(|input| input.membership()).collect();
        let membership = RecordingCodec::new().derive(identity, &records, operation)?;
        match payloads {
            PayloadResidency::Materialized(entities) => Self::materialized(membership, entities),
            PayloadResidency::Graph => Ok(Self::graph(membership)),
        }
    }
}

impl serde::Serialize for ExecutionCollection {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ExecutionCollection", 3)?;
        state.serialize_field("entities", self.resident_entities())?;
        state.serialize_field("count", &self.count())?;
        state.serialize_field("coverage", &self.coverage())?;
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{
        collect_query_stream, ExecutionEvent, ExecutionResult, QueryStream, StreamConsumeOpts,
    };

    fn identity() -> CollectionIdentity {
        CollectionIdentity {
            catalog: [1; 32],
            expression: [2; 32],
            epoch: 7,
        }
    }
    fn row(id: &str) -> CachedEntity {
        CachedEntity::from_decoded(
            Ref::new("Row", id),
            Default::default(),
            Default::default(),
            0,
            crate::EntityCompleteness::Complete,
        )
    }
    fn complete(collection: ExecutionCollection) -> ExecutionResult {
        ExecutionResult {
            collection,
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: crate::execution::ExecutionSource::Live,
            stats: crate::execution::ExecutionStats {
                cache_misses: 999,
                ..Default::default()
            },
            request_fingerprints: vec![],
            operations: Default::default(),
        }
    }

    #[test]
    fn filtering_shares_payloads_and_retains_occurrences() {
        let input = ExecutionCollection::observe(
            identity(),
            vec![row("a"), row("b"), row("a")],
            Observation::ExactOutput { decoded: 3 },
        )
        .unwrap();
        let filtered = input.filter(&"predicate", &[0, 2], &[]).unwrap();
        assert_eq!(filtered.count(), 2);
        assert_eq!(filtered.coverage(), ResultCoverage::Complete);
        assert!(std::ptr::eq(&input.entities[0], &filtered.entities[0]));
        assert!(std::ptr::eq(&input.entities[2], &filtered.entities[1]));
        let copy = filtered.clone();
        assert!(std::ptr::eq(&copy.entities[0], &filtered.entities[0]));
    }

    #[test]
    fn explicit_page_selection_preserves_uncertainty_and_shares_payloads() {
        for observation in [
            Observation::ExactOutput { decoded: 3 },
            Observation::UnprovenPage,
        ] {
            let source = ExecutionCollection::observe(
                identity(),
                vec![row("a"), row("b"), row("a")],
                observation,
            )
            .unwrap();
            let page = source.filter(&"page", &[1, 2], &[]).unwrap();
            assert_eq!(page.count(), 2);
            assert_eq!(page.coverage(), source.coverage());
            assert!(!page.is_graph_backed());
            let rows = page.materialize(Demand::Observed).unwrap();
            assert!(std::ptr::eq(&source.entities[1], &rows[0]));
            assert!(std::ptr::eq(&source.entities[2], &rows[1]));
            assert_eq!(
                page.materialize(Demand::Whole).is_ok(),
                source.materialize(Demand::Whole).is_ok()
            );
            assert_eq!(source.count(), 3);
        }
    }

    #[test]
    fn residency_cannot_change_logical_membership() {
        let source = ExecutionCollection::observe(
            identity(),
            vec![row("a"), row("b"), row("a")],
            Observation::ExactOutput { decoded: 3 },
        )
        .unwrap();
        let graph = ExecutionCollection::graph(source.membership.clone());
        assert_eq!(graph.count(), 3);
        assert!(graph.entities.is_empty());
        assert!(graph
            .with_materialization(vec![row("a"), row("a"), row("b")].into())
            .is_err());
        assert!(graph
            .with_materialization(vec![row("a"), row("b")].into())
            .is_err());
        let loaded = graph.with_materialization(source.entities.clone()).unwrap();
        assert!(std::ptr::eq(&loaded.entities[0], &source.entities[0]));
    }

    #[tokio::test]
    async fn delivery_uses_recorded_sequence_not_telemetry_or_last_page() {
        let source = ExecutionCollection::observe(
            identity(),
            vec![row("a"), row("b"), row("a")],
            Observation::ExactOutput { decoded: 3 },
        )
        .unwrap();
        let events = vec![
            Ok(ExecutionEvent::Page {
                entities: source.entities.select([0, 1]).unwrap(),
                stats: Default::default(),
            }),
            Ok(ExecutionEvent::Page {
                entities: source.entities.select([2]).unwrap(),
                stats: Default::default(),
            }),
            Ok(ExecutionEvent::Complete(complete(
                ExecutionCollection::graph(source.membership.clone()),
            ))),
        ];
        let mut stream: QueryStream<'_> = Box::pin(futures_util::stream::iter(events));
        let result = collect_query_stream(&mut stream, &StreamConsumeOpts::default())
            .await
            .unwrap();
        assert_eq!(result.count(), 3);
        assert_eq!(result.stats.cache_misses, 999);
        assert!(std::ptr::eq(&result.entities()[2], &source.entities[2]));
    }

    #[tokio::test]
    async fn missing_duplicate_or_late_terminal_fails_closed() {
        let source = ExecutionCollection::observe(
            identity(),
            vec![row("a")],
            Observation::ExactOutput { decoded: 1 },
        )
        .unwrap();
        for events in [
            vec![],
            vec![
                Ok(ExecutionEvent::Complete(complete(source.clone()))),
                Ok(ExecutionEvent::Complete(complete(source.clone()))),
            ],
            vec![
                Ok(ExecutionEvent::Complete(complete(source.clone()))),
                Ok(ExecutionEvent::Page {
                    entities: vec![].into(),
                    stats: Default::default(),
                }),
            ],
            vec![Ok(ExecutionEvent::Complete(complete(
                ExecutionCollection::graph(source.membership.clone()),
            )))],
        ] {
            let mut stream: QueryStream<'_> = Box::pin(futures_util::stream::iter(events));
            assert!(
                collect_query_stream(&mut stream, &StreamConsumeOpts::default())
                    .await
                    .is_err()
            );
        }
    }
}

#[cfg(test)]
pub(crate) fn test_collection(
    entities: Vec<CachedEntity>,
    coverage: ResultCoverage,
) -> ExecutionCollection {
    // Fixture adapter states deliberately exercise all three observations.
    let observation = match coverage {
        ResultCoverage::Complete => Observation::ExactOutput {
            decoded: entities.len(),
        },
        ResultCoverage::Partial => Observation::MoreAvailable,
        ResultCoverage::Unknown => Observation::UnprovenPage,
    };
    ExecutionCollection::observe(
        CollectionIdentity {
            catalog: [7; 32],
            expression: [8; 32],
            epoch: 0,
        },
        entities,
        observation,
    )
    .unwrap()
}

#[cfg(test)]
mod occurrence_properties {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]
        #[test]
        fn nested_flatmap_delivery_and_reload_conserve_occurrence_order(
            children in prop::collection::vec(prop::collection::vec(0u8..8, 0..12), 0..8),
            unknown_parent in any::<bool>(),
            partial_child in any::<bool>(),
            split in any::<usize>(),
        ) {
            let parent_rows = (0..children.len()).map(|i| CachedEntity::new(Ref::new("Parent", i.to_string()), 0)).collect();
            let parent = ExecutionCollection::observe(CollectionIdentity::for_untyped_observation(&"parent").unwrap(), parent_rows,
                if unknown_parent { Observation::UnprovenPage } else { Observation::ExactOutput { decoded: children.len() } }).unwrap();
            let outputs = children.iter().enumerate().map(|(i, values)| ExecutionCollection::observe(
                parent.membership().identity().derived(&i).unwrap(),
                values.iter().map(|value| CachedEntity::new(Ref::new("Child", value.to_string()), 0)).collect(),
                if partial_child { Observation::MoreAvailable } else { Observation::ExactOutput { decoded: values.len() } }).unwrap()).collect::<Vec<_>>();
            let result = parent.flat_map(&"children", &outputs).unwrap();
            let expected = children.iter().flatten().map(|v| Ref::new("Child", v.to_string())).collect::<Vec<_>>();
            prop_assert_eq!(result.membership().observed().iter().cloned().collect::<Vec<_>>(), expected);
            let complete = !unknown_parent && (!partial_child || children.is_empty());
            prop_assert_eq!(result.materialize(Demand::Whole).is_ok(), complete);
            let offset = split % (result.count() + 1);
            let first = result.delivery(0..offset).unwrap();
            let second = result.delivery(offset..result.count()).unwrap();
            prop_assert_eq!(first.membership(), result.membership());
            prop_assert_eq!(second.membership(), result.membership());
            let loaded = ExecutionCollection::graph(result.membership().clone()).with_materialization(SharedRows::concat([first.resident_entities(), second.resident_entities()])).unwrap();
            for index in 0..result.count() {
                prop_assert!(std::ptr::eq(&loaded.resident_entities()[index], &result.resident_entities()[index]));
            }
        }
    }
}
