//! Revision-local authorized facts. Nothing in this index is model evidence until selected.
#[cfg(test)]
use super::search_rank::{LexicalIndex, RankedDocument};
use super::{
    evidence::CatalogEvidence,
    search_graph::{Edge, Relationship, SearchGraph},
    DiscoveryMatcherError,
};
use crate::discovery_store::{DiscoveryAuthorization, RetrievalReceipt, RetrievedCapability};
use plasm_core::{
    catalog_discovery::capability_documents,
    prerequisites::{
        project_input_source_candidates, CapabilityRef, DeploymentBindings, InputSourceBinding,
    },
    schema::CapabilityKind,
    CGS,
};
#[cfg(test)]
use plasm_core::{
    catalog_discovery::capability_observation_meaning,
    schema::{
        CapabilitySchema, InputFieldSchema, InputFieldWire, InputType, OutputType,
        RelationMaterialization,
    },
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) struct CatalogueSearch {
    pub(super) candidates: BTreeMap<CapabilityRef, RetrievedCapability>,
    /// Stable question-local reference labels for this authorized revision.
    pub(super) labels: BTreeMap<CapabilityRef, String>,
    #[cfg(test)]
    pub(super) ranking: LexicalIndex<CapabilityRef>,
    pub(super) graph: SearchGraph<CapabilityRef>,
    pub(super) input_bindings: BTreeMap<String, InputSourceBinding>,
}

#[cfg(test)]
fn input_fields(fields: &[InputFieldSchema], cgs: &CGS, text: &mut BTreeSet<String>) {
    for field in fields {
        text.extend(field.description.iter().cloned());
        match &field.wire {
            InputFieldWire::Registry(name) => {
                if let Some(value) = cgs.values.get(name.as_str()) {
                    text.insert(value.description.clone());
                }
            }
            InputFieldWire::Inline(input) => input_text(input, cgs, text),
        }
    }
}

#[cfg(test)]
fn input_text(input: &InputType, cgs: &CGS, text: &mut BTreeSet<String>) {
    match input {
        InputType::None | InputType::Value { .. } => {}
        InputType::Object { fields, .. } => input_fields(fields, cgs, text),
        InputType::Array { element_type, .. } => input_text(element_type, cgs, text),
        InputType::Union { variants } => {
            for variant in variants {
                input_fields(&variant.fields, cgs, text);
            }
        }
    }
}

/// Ranking fields retain authored roles. The rendered operation contract also
/// contains inputs, so indexing it here would count inputs twice at two weights.
#[cfg(test)]
fn operation_meaning(cap: &CapabilitySchema) -> String {
    let mut text = cap.description.clone();
    if let Some(OutputType::SideEffect { description }) =
        cap.output_schema.as_ref().map(|o| &o.output_type)
    {
        if description != &cap.description {
            text.push('\n');
            text.push_str(description);
        }
    }
    text
}

/// A parent GET can expose its declared embedded relations. Do not transfer
/// those observations to every operation sharing the owning entity.
#[cfg(test)]
fn embedded_observations(cap: &CapabilitySchema, cgs: &CGS) -> String {
    if cap.kind != CapabilityKind::Get
        || cap.output_schema.as_ref().is_some_and(|output| {
            !matches!(&output.output_type, OutputType::Entity { entity_type } if entity_type.as_str() == cap.domain.as_str())
        })
    {
        return String::new();
    }
    cgs.entities[&cap.domain]
        .relations
        .values()
        .filter(|relation| {
            matches!(
                relation.materialize.as_ref(),
                Some(
                    RelationMaterialization::FromParentGet { .. }
                        | RelationMaterialization::PreferFromParentGet { .. }
                )
            )
        })
        .map(|relation| relation.description.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

impl CatalogueSearch {
    pub(crate) fn authorized_capabilities(
        &self,
        authorization: &DiscoveryAuthorization,
    ) -> BTreeSet<CapabilityRef> {
        self.candidates
            .keys()
            .filter(|reference| authorization.permits(reference))
            .cloned()
            .collect()
    }
    pub fn new(
        catalogs: &BTreeMap<String, CGS>,
        _deployments: &DeploymentBindings,
        allowed: &DiscoveryAuthorization,
    ) -> Result<Self, DiscoveryMatcherError> {
        let borrowed = catalogs
            .iter()
            .map(|(key, cgs)| (key.clone(), cgs))
            .collect();
        let mut candidates = BTreeMap::new();
        #[cfg(test)]
        let mut documents = Vec::new();
        for (catalog, cgs) in catalogs {
            if !allowed.catalogs.contains(catalog) {
                continue;
            }
            for document in capability_documents(cgs).map_err(DiscoveryMatcherError::from)? {
                let reference = CapabilityRef {
                    catalog: catalog.clone(),
                    capability: document.capability.clone(),
                };
                if !allowed.permits(&reference) {
                    continue;
                }
                #[cfg(test)]
                {
                    let cap = &cgs.capabilities[reference.capability.as_str()];
                    // Reusing one annotation across slots is not additional
                    // semantic evidence. Full contracts retain every input seat.
                    let mut inputs = BTreeSet::new();
                    for fields in [
                        &cap.inputs.scope.0,
                        &cap.inputs.selection.0,
                        &cap.inputs.controls.0,
                    ] {
                        input_fields(fields, cgs, &mut inputs);
                    }
                    for input in [&cap.inputs.arguments, &cap.inputs.payload]
                        .into_iter()
                        .flatten()
                    {
                        inputs.extend(input.description.iter().cloned());
                        input_text(&input.input_type, cgs, &mut inputs);
                    }
                    documents.push(RankedDocument {
                        key: reference.clone(),
                        fields: [
                            operation_meaning(cap),
                            format!(
                                "{} {} {}",
                                document.collection.meaning,
                                capability_observation_meaning(cgs, cap)
                                    .map_err(DiscoveryMatcherError::from)?,
                                embedded_observations(cap, cgs)
                            ),
                            inputs.into_iter().collect::<Vec<_>>().join(" "),
                        ],
                    });
                }
                let id = plasm_core::catalog_discovery::content_hash(
                    &serde_json::to_vec(&reference).map_err(DiscoveryMatcherError::Packing)?,
                );
                candidates.insert(
                    reference.clone(),
                    RetrievedCapability {
                        id,
                        reference,
                        document,
                        admissions: BTreeSet::new(),
                    },
                );
            }
        }
        let references: Vec<_> = candidates.keys().cloned().collect();
        let mut edges = BTreeSet::new();
        let mut input_bindings = BTreeMap::new();
        let sources =
            project_input_source_candidates(&borrowed, &references, &|r| allowed.permits(r))?;
        for source in sources {
            // Only actual input bindings nominate suppliers. Membership evidence
            // does not establish a producer-consumer assignment.
            for binding in source.bindings {
                let provenance = plasm_core::catalog_discovery::content_hash(
                    &serde_json::to_vec(&binding).map_err(DiscoveryMatcherError::Packing)?,
                );
                edges.insert(Edge {
                    source: binding.provider.clone(),
                    consumer: binding.consumer.clone(),
                    relationship: Relationship::SuppliesInput,
                    provenance: provenance.clone(),
                });
                input_bindings.insert(provenance, binding);
            }
        }
        let mut creators = BTreeMap::<(String, String), Vec<CapabilityRef>>::new();
        for reference in candidates.keys() {
            let cap = &catalogs[&reference.catalog].capabilities[reference.capability.as_str()];
            if cap.kind == CapabilityKind::Create {
                creators
                    .entry((reference.catalog.clone(), cap.domain.to_string()))
                    .or_default()
                    .push(reference.clone());
            }
        }
        for reference in candidates.keys() {
            let cap = &catalogs[&reference.catalog].capabilities[reference.capability.as_str()];
            if !matches!(
                cap.kind,
                CapabilityKind::Get | CapabilityKind::Query | CapabilityKind::Search
            ) {
                continue;
            }
            for creator in creators
                .get(&(reference.catalog.clone(), cap.domain.to_string()))
                .into_iter()
                .flatten()
            {
                edges.insert(Edge {
                    source: creator.clone(),
                    consumer: reference.clone(),
                    relationship: Relationship::MayCreateResource,
                    provenance: format!("declared_create:{}:{}", reference.catalog, cap.domain),
                });
            }
        }
        let graph =
            SearchGraph::new(candidates.keys().cloned().collect(), edges).map_err(|error| {
                match error {
                    super::search_graph::GraphError::Unauthorized(reference) => {
                        DiscoveryMatcherError::UnauthorizedWitness(reference)
                    }
                }
            })?;
        #[cfg(test)]
        let ranking = LexicalIndex::new(documents)
            .map_err(|_| DiscoveryMatcherError::DuplicateCapabilityReference)?;
        let labels = candidates
            .keys()
            .enumerate()
            .map(|(i, reference)| (reference.clone(), format!("c{i}")))
            .collect();
        Ok(Self {
            candidates,
            labels,
            #[cfg(test)]
            ranking,
            graph,
            input_bindings,
        })
    }

    /// Project only an admitted path. No catalogue-wide object is serialized into
    /// the classifier request. The existing projector owns complete contracts.
    pub fn evidence(
        &self,
        generation: &str,
        references: &BTreeSet<CapabilityRef>,
        catalogs: &BTreeMap<String, CGS>,
        deployments: &DeploymentBindings,
        allowed: &DiscoveryAuthorization,
    ) -> Result<(RetrievalReceipt, CatalogEvidence), DiscoveryMatcherError> {
        let candidates = references
            .iter()
            .map(|reference| {
                self.candidates.get(reference).cloned().ok_or_else(|| {
                    DiscoveryMatcherError::MissingCatalogueEvidence(reference.clone())
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let retrieval = RetrievalReceipt {
            generation: generation.into(),
            candidates,
            lexical_count: 0,
            vector_count: 0,
            lexical_truncated: false,
            vector_truncated: false,
        };
        let evidence = CatalogEvidence::from_catalogs(&retrieval, catalogs, deployments, &|r| {
            allowed.permits(r)
        })?;
        Ok((retrieval, evidence))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authored_action_purpose_survives_a_nonspecific_effect_description() {
        let mut cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/discovery_reference_index"),
        )
        .unwrap();
        cgs.capabilities.retain(|name, _| name.as_str() == "opaque");
        let cap = cgs.capabilities.get_mut("opaque").unwrap();
        cap.description = "Polarize the specimen.".into();
        cap.output_schema.as_mut().unwrap().output_type = OutputType::SideEffect {
            description: "Changes the recorded state.".into(),
        };
        let catalogs = BTreeMap::from([("matrix".into(), cgs)]);
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let index = CatalogueSearch::new(&catalogs, &Default::default(), &allowed).unwrap();
        assert_eq!(index.ranking.rank("Polarize").len(), 1);
    }

    #[test]
    fn repeated_input_annotations_do_not_promote_a_weaker_operation() {
        let mut cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/discovery_reference_index"),
        )
        .unwrap();
        cgs.capabilities.get_mut("source_read").unwrap().description =
            "Inspect calibration.".into();
        let mut input = cgs.capabilities["prepare"]
            .inputs
            .arguments
            .clone()
            .unwrap();
        let InputType::Object { fields, .. } = &mut input.input_type else {
            panic!("fixture object")
        };
        fields[0].description = Some("Calibration".into());
        let field = fields[0].clone();
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let target = CapabilityRef {
            catalog: "matrix".into(),
            capability: "source_read".into(),
        };
        for count in [1, 64] {
            let mut input = input.clone();
            let InputType::Object { fields, .. } = &mut input.input_type else {
                unreachable!()
            };
            *fields = (0..count)
                .map(|n| {
                    let mut copy = field.clone();
                    copy.name = format!("seat_{n}");
                    copy
                })
                .collect();
            cgs.capabilities.get_mut("opaque").unwrap().inputs.arguments = Some(input);
            let catalogs = BTreeMap::from([("matrix".into(), cgs.clone())]);
            let index = CatalogueSearch::new(&catalogs, &Default::default(), &allowed).unwrap();
            assert_eq!(
                index.ranking.rank("calibration")[0],
                target,
                "duplicate count {count}"
            );
        }
    }

    #[test]
    fn ranking_keeps_input_meaning_out_of_operation_but_preserves_full_contracts() {
        let mut cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/discovery_reference_index"),
        )
        .unwrap();
        let cap = cgs.capabilities.get_mut("process").unwrap();
        cap.description = "Produce a spectrum.".into();
        // Alter only input prose, not the operation or any declared relationship.
        let original = operation_meaning(cap);
        let input = cap
            .inputs
            .arguments
            .as_mut()
            .or(cap.inputs.payload.as_mut())
            .unwrap();
        input.description = Some("Optional archive rating criterion.".into());
        assert_eq!(operation_meaning(cap), original);
        let documents = capability_documents(&cgs).unwrap();
        let document = documents
            .iter()
            .find(|d| d.capability == "process")
            .unwrap();
        assert!(document
            .operation
            .contract
            .contains("Optional archive rating criterion"));
        let catalogs = BTreeMap::from([("matrix".into(), cgs)]);
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let index = CatalogueSearch::new(&catalogs, &Default::default(), &allowed).unwrap();
        let reference = CapabilityRef {
            catalog: "matrix".into(),
            capability: "process".into(),
        };
        assert_eq!(index.candidates[&reference].document, *document);
        assert!(index.ranking.rank("criterion").contains(&reference));
        assert_eq!(index.ranking.rank("spectrum")[0], reference);
        let denied = CatalogueSearch::new(
            &catalogs,
            &Default::default(),
            &DiscoveryAuthorization::catalogs(BTreeSet::new()),
        )
        .unwrap();
        assert!(denied.ranking.rank("spectrum criterion").is_empty());
    }

    #[test]
    fn declared_output_meaning_is_ranked_without_leaking_to_opaque_peers() {
        let mut cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/discovery_reference_index"),
        )
        .unwrap();
        cgs.values.get_mut("result").unwrap().description = "Spectral amplitude.".into();
        let documents = capability_documents(&cgs).unwrap();
        let catalogs = BTreeMap::from([("matrix".into(), cgs)]);
        let allowed = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        let index = CatalogueSearch::new(&catalogs, &Default::default(), &allowed).unwrap();
        let ranked = index.ranking.rank("spectral amplitude");
        assert_eq!(
            ranked,
            vec![CapabilityRef {
                catalog: "matrix".into(),
                capability: "process".into()
            }]
        );
        for candidate in index.candidates.values() {
            assert_eq!(
                &candidate.document,
                documents
                    .iter()
                    .find(|d| d.capability == candidate.reference.capability)
                    .unwrap()
            );
        }
    }

    #[test]
    fn effect_only_meaning_is_searchable_without_input_expansion() {
        let mut cgs = plasm_core::loader::load_schema_dir(
            &std::path::Path::new(crate::test_manifest_dir())
                .join("../../fixtures/schemas/discovery_reference_index"),
        )
        .unwrap();
        let cap = cgs.capabilities.get_mut("prepare").unwrap();
        cap.description.clear();
        cap.output_schema.as_mut().unwrap().output_type = OutputType::SideEffect {
            description: "Stages prepared material.".into(),
        };
        assert_eq!(operation_meaning(cap).trim(), "Stages prepared material.");
    }
}
