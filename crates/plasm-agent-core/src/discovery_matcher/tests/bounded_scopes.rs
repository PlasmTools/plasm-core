//! Experimental bounded-neighbour decomposition. Deliberately test-only: edge
//! coverage does not establish that negative judgments compose across pages.
use super::*;

pub(super) fn matrix(
    neighbours: usize,
    description_bytes: usize,
) -> (RetrievalReceipt, CatalogEvidence) {
    let (mut catalogs, mut retrieval) = super::reference_index::matrix();
    let cgs = catalogs.get_mut("matrix").unwrap();
    cgs.entities
        .get_mut("Source")
        .unwrap()
        .fields
        .shift_remove("parent");
    cgs.entities.get_mut("Source").unwrap().relations.clear();
    let source = cgs.capabilities["source_read"].clone();
    cgs.capabilities
        .retain(|name, _| name.as_str() == "prepare");
    for i in 0..neighbours {
        let mut cap = source.clone();
        let name = format!("source_{i:03}");
        cap.name = name.clone().into();
        cap.description = format!(
            "Distinct acquisition {i}. {}",
            "x".repeat(description_bytes)
        );
        cgs.capabilities.insert(name.into(), cap);
    }
    retrieval.candidates = plasm_core::catalog_discovery::capability_documents(cgs)
        .unwrap()
        .into_iter()
        .map(|document| crate::discovery_store::RetrievedCapability {
            id: document.capability.clone(),
            reference: CapabilityRef {
                catalog: "matrix".into(),
                capability: document.capability.clone(),
            },
            document,
            admissions: BTreeSet::from(["lexical".into()]),
        })
        .collect();
    let evidence =
        CatalogEvidence::from_catalogs(&retrieval, &catalogs, &Default::default(), &|_| true)
            .unwrap();
    retrieval
        .candidates
        .retain(|c| c.reference.capability == "prepare");
    (retrieval, evidence)
}

fn ids(evidence: &CatalogEvidence) -> BTreeMap<CapabilityRef, String> {
    evidence
        .capabilities
        .keys()
        .enumerate()
        .map(|(i, r)| (r.clone(), format!("c{i}")))
        .collect()
}

/// Test a minimal deterministic partition against the real projector/codec and
/// byte admission. Keep all bindings for a neighbour together; never truncate.
fn partition(
    retrieval: &RetrievalReceipt,
    evidence: &CatalogEvidence,
) -> Result<Vec<IssuedBatch>, DiscoveryMatcherError> {
    let intent =
        IntentProvenance::from_turns(["Prepare material while preserving originals.".into()])
            .unwrap();
    let target = &retrieval.candidates[0].reference;
    let full = witnesses::project(
        target,
        evidence,
        &ids(evidence),
        &witnesses::value(&evidence.definitions)?,
    )?;
    let cards = serde_json::to_value(&full.cards).unwrap();
    let neighbours: BTreeSet<CapabilityRef> = cards
        .as_object()
        .unwrap()
        .values()
        .map(|c| serde_json::from_value(c["capability"]["reference"].clone()).unwrap())
        .filter(|r| r != target)
        .collect();
    let render = |allowed: &BTreeSet<CapabilityRef>| {
        let mut local = evidence.clone();
        local.bindings.retain(|b| {
            b.endpoints()
                .iter()
                .all(|e| &e.capability == target || allowed.contains(&e.capability))
        });
        local
            .bindings
            .sort_by_cached_key(|b| serde_json::to_string(b).unwrap());
        super::super::issue_batches(RELEVANCE_MODEL, &intent, retrieval, &local)
            .map(|mut b| b.remove(0))
    };
    let mut page = BTreeSet::new();
    let mut current = render(&page)?;
    let mut pages = Vec::new();
    for neighbour in neighbours {
        let mut next = page.clone();
        next.insert(neighbour.clone());
        match render(&next) {
            Ok(batch) => {
                page = next;
                current = batch;
            }
            Err(DiscoveryMatcherError::ScopeLimit { .. }) if !page.is_empty() => {
                pages.push(current);
                page = BTreeSet::from([neighbour]);
                current = render(&page)?;
            }
            Err(error) => return Err(error),
        }
    }
    pages.push(current);
    Ok(pages)
}

#[test]
fn partition_preserves_every_direct_composition_binding_with_fixed_limits() {
    for (neighbours, description_bytes) in [(1, 0), (7, 0), (8, 0), (16, 0), (32, 0), (8, 3000)] {
        let (retrieval, evidence) = matrix(neighbours, description_bytes);
        let pages = partition(&retrieval, &evidence).unwrap();
        let target = &retrieval.candidates[0].reference;
        let full = witnesses::project(
            target,
            &evidence,
            &ids(&evidence),
            &witnesses::value(&evidence.definitions).unwrap(),
        )
        .unwrap();
        let full_cards = serde_json::to_value(&full.cards).unwrap();
        let target_id = &ids(&evidence)[target];
        let expected: BTreeSet<_> = full
            .bindings
            .iter()
            .map(|b| serde_json::to_string(b).unwrap())
            .collect();
        let mut observed = BTreeSet::new();
        for page in &pages {
            assert!(page.body().len() <= witnesses::MAX_BYTES);
            let body: serde_json::Value = serde_json::from_str(page.body()).unwrap();
            assert!(
                body["state"]["capabilities"].as_object().unwrap().len()
                    <= witnesses::MAX_CAPABILITIES
            );
            assert_eq!(
                body["state"]["capabilities"][target_id],
                full_cards[target_id]
            );
            for binding in body["state"]["bindings"].as_array().unwrap() {
                observed.insert(serde_json::to_string(binding).unwrap());
            }
        }
        assert_eq!(observed, expected);
        let mut permuted = evidence.clone();
        permuted.bindings.reverse();
        assert_eq!(
            pages.iter().map(|b| b.body()).collect::<Vec<_>>(),
            partition(&retrieval, &permuted)
                .unwrap()
                .iter()
                .map(|b| b.body())
                .collect::<Vec<_>>()
        );
        println!("CONTROL neighbours={neighbours} description_bytes={description_bytes} requests={} total_bytes={}", pages.len(), pages.iter().map(|b| b.body().len()).sum::<usize>());
    }
}

#[test]
fn page_negative_judgments_do_not_prove_union_is_unrelated() {
    let (retrieval, evidence) = matrix(16, 0);
    let pages = partition(&retrieval, &evidence).unwrap();
    let bodies: Vec<serde_json::Value> = pages
        .iter()
        .map(|b| serde_json::from_str(b.body()).unwrap())
        .collect();
    // A composed outcome may need two different acquisitions. Both contracts
    // survive, but no individual page supplies the combined evidence.
    let has = |body: &serde_json::Value, name: &str| {
        body["state"]["capabilities"]
            .as_object()
            .unwrap()
            .values()
            .any(|c| c["capability"]["reference"]["capability"] == name)
    };
    assert!(bodies.iter().any(|b| has(b, "source_000")));
    assert!(bodies.iter().any(|b| has(b, "source_015")));
    assert!(!bodies
        .iter()
        .any(|b| has(b, "source_000") && has(b, "source_015")));
    // Consequently, "exclude iff every page says unrelated" is not a valid
    // completeness rule. This test does not simulate or claim model accuracy.
}

#[test]
fn an_indivisible_contract_still_fails_without_truncation() {
    let (retrieval, evidence) = matrix(1, witnesses::MAX_BYTES);
    assert!(matches!(
        partition(&retrieval, &evidence),
        Err(DiscoveryMatcherError::ScopeLimit { .. })
    ));
}
