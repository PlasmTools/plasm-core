//! Relation membership derives from ordered producer records, never paging metadata.
use plasm_core::collection_codec::{
    CollectionCodec, CollectionFault, Observation, RecordingCodec, Transform,
};
use plasm_runtime::execution::{ExecutionCollection, PayloadResidency};
use plasm_runtime::ExecutionResult;

pub(crate) fn embedded_collection<'a>(
    source: &ExecutionResult,
    parents: &plasm_core::collection_codec::SharedRows<plasm_runtime::CachedEntity>,
    relation: &str,
    target: &str,
    returned: impl IntoIterator<Item = &'a plasm_core::Ref>,
    read_cap: Option<usize>,
) -> Result<ExecutionCollection, CollectionFault> {
    let codec = RecordingCodec::new();
    let prefix = codec.validate_prefix(
        &mut source.collection.membership().observed().iter(),
        &mut parents.iter().map(|parent| &parent.reference),
    )?;
    if prefix.discarded() != 0 {
        return Err(CollectionFault::Conservation);
    }
    let mut children = Vec::with_capacity(parents.len());
    for (position, parent) in parents.iter().enumerate() {
        let identity = source.collection.membership().identity().derived(&(
            relation,
            &parent.reference,
            position,
        ))?;
        let record = match parent.relations.get(relation) {
            Some(refs) => {
                if refs.iter().any(|r| r.entity_type.as_str() != target) {
                    return Err(CollectionFault::Conservation);
                }
                refs.record().clone()
            }
            None => codec.record(identity, vec![], Observation::UnprovenPage)?,
        };
        children.push(ExecutionCollection::graph(record));
    }
    let mut collection = source.collection.flat_map(&(relation, target), &children)?;
    if let Some(count) = read_cap {
        collection = ExecutionCollection::derive(
            collection
                .membership()
                .identity()
                .derived(&("take", count))?,
            &[&collection],
            Transform::Take(count),
            PayloadResidency::Graph,
        )?;
    }
    let prefix = codec.validate_prefix(
        &mut collection.membership().observed().iter(),
        &mut returned.into_iter(),
    )?;
    if prefix.discarded() != 0 {
        return Err(CollectionFault::Conservation);
    }
    Ok(collection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plasm_core::{
        collection_codec::{CollectionIdentity, Demand, ResultCoverage},
        row_contract::RelationMembership,
        Ref,
    };
    use plasm_runtime::CachedEntity;
    fn source(parents: Vec<CachedEntity>, observation: Observation) -> ExecutionResult {
        ExecutionResult {
            collection: ExecutionCollection::observe(
                CollectionIdentity::for_untyped_observation(&"parent_fixture").unwrap(),
                parents,
                observation,
            )
            .unwrap(),
            has_more: false,
            pagination_resume: None,
            paging_handle: None,
            source: plasm_runtime::ExecutionSource::Live,
            stats: Default::default(),
            request_fingerprints: vec![],
            operations: Default::default(),
        }
    }
    fn parent(refs: Vec<Ref>) -> CachedEntity {
        let mut row = CachedEntity::new(Ref::new("Parent", "p1"), 0);
        let count = refs.len();
        row.relations.insert(
            "children".into(),
            RelationMembership::from_record(
                RecordingCodec::new()
                    .record(
                        CollectionIdentity::for_untyped_observation(&"child_fixture").unwrap(),
                        refs,
                        Observation::ExactOutput { decoded: count },
                    )
                    .unwrap(),
            ),
        );
        row
    }
    #[test]
    fn embedded_prefix_requires_declared_operator_and_exact_occurrences() {
        let refs = vec![
            Ref::new("Child", "a"),
            Ref::new("Child", "a"),
            Ref::new("Child", "b"),
        ];
        let row = parent(refs.clone());
        let input = source(vec![row.clone()], Observation::ExactOutput { decoded: 1 });
        for count in 0..=refs.len() {
            let result = embedded_collection(
                &input,
                &vec![row.clone()].into(),
                "children",
                "Child",
                &refs[..count],
                Some(count),
            )
            .unwrap();
            assert_eq!(result.coverage(), ResultCoverage::Complete);
            assert_eq!(
                result.membership().observed().iter().collect::<Vec<_>>(),
                refs[..count].iter().collect::<Vec<_>>()
            );
            if count < refs.len() {
                assert!(matches!(
                    embedded_collection(
                        &input,
                        &vec![row.clone()].into(),
                        "children",
                        "Child",
                        &refs[..count],
                        None
                    ),
                    Err(CollectionFault::Conservation)
                ));
            }
        }
        for bad in [
            vec![refs[0].clone(), refs[2].clone(), refs[2].clone()],
            vec![refs[2].clone(), refs[0].clone(), refs[0].clone()],
        ] {
            assert!(matches!(
                embedded_collection(
                    &input,
                    &vec![row.clone()].into(),
                    "children",
                    "Child",
                    &bad,
                    None
                ),
                Err(CollectionFault::Conservation)
            ));
        }
        assert!(matches!(
            embedded_collection(
                &input,
                &Vec::new().into(),
                "children",
                "Child",
                std::iter::empty(),
                None
            ),
            Err(CollectionFault::Conservation)
        ));
    }
    #[test]
    fn incomplete_parent_and_missing_edge_cannot_become_whole_but_delivery_flags_are_irrelevant() {
        let refs = vec![Ref::new("Child", "a")];
        let row = parent(refs.clone());
        for observation in [Observation::MoreAvailable, Observation::UnprovenPage] {
            let input = source(vec![row.clone()], observation);
            let output = embedded_collection(
                &input,
                &vec![row.clone()].into(),
                "children",
                "Child",
                &refs,
                None,
            )
            .unwrap();
            assert!(RecordingCodec::new()
                .materialize(output.membership(), Demand::Whole)
                .is_err());
        }
        let mut input = source(vec![row.clone()], Observation::ExactOutput { decoded: 1 });
        input.has_more = true;
        input.paging_handle = Some(plasm_core::PagingHandle::mint_monotonic(1));
        assert_eq!(
            embedded_collection(
                &input,
                &vec![row.clone()].into(),
                "children",
                "Child",
                &refs,
                None
            )
            .unwrap()
            .coverage(),
            ResultCoverage::Complete
        );
        assert_eq!(
            embedded_collection(
                &input,
                &vec![row].into(),
                "missing",
                "Child",
                std::iter::empty(),
                None
            )
            .unwrap()
            .coverage(),
            ResultCoverage::Unknown
        );
    }
    #[test]
    fn decoding_storage_preserves_proof_and_public_values_cannot_mint_it() {
        use plasm_compile::{EntityDecoder, PathExpr, RelationDecoder};
        use plasm_core::row_contract::{RowCodec, RowRecord};
        let cgs = plasm_core::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/hydration_boundary_matrix"),
        )
        .unwrap();
        let entity = cgs.get_entity("Folder").unwrap();
        let plasm_core::RelationMaterialization::FromParentGet { path, .. } =
            entity.relations["wire_notes"].materialize.as_ref().unwrap()
        else {
            panic!()
        };
        let child = plasm_compile::entity_decoder_for_from_parent_get_target(
            cgs.get_entity("Note").unwrap(),
            entity,
            plasm_compile::path_expr_from_json_segments(path).unwrap(),
        );
        let decoder =
            EntityDecoder::new("Folder", PathExpr::empty()).with_relations(vec![RelationDecoder {
                relation: "wire_notes".into(),
                decoder: child,
                cardinality: plasm_core::Cardinality::Many,
            }]);
        for ids in [vec![], vec![1, 2, 2]] {
            let wire = serde_json::json!({"id":"root","payload":{"items":ids.iter().map(|id|serde_json::json!({"id":id})).collect::<Vec<_>>()}});
            let decoded =
                plasm_compile::decode_entities_with_cgs(&decoder, &wire, Some(&cgs)).unwrap();
            let cached = CachedEntity::from_row(
                &RowRecord::capture(&decoded[0]),
                0,
                plasm_runtime::EntityCompleteness::Complete,
            );
            let stored: CachedEntity =
                serde_json::from_slice(&serde_json::to_vec(&cached).unwrap()).unwrap();
            assert_eq!(stored.relations, cached.relations);
            assert!(stored.relations["wire_notes"].is_exhaustive());
            let values = RowCodec::new(Some(&cgs)).values(&stored);
            let public = RowCodec::new(Some(&cgs))
                .decode_values("Folder", &values)
                .unwrap();
            let public =
                CachedEntity::from_row(&public, 0, plasm_runtime::EntityCompleteness::Complete);
            assert!(!public.relations["wire_notes"].is_exhaustive());
        }
    }
}
