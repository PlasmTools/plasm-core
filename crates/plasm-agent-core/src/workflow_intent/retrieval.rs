//! Per-requirement retrieval through the existing authorized PostgreSQL path.
use super::{contract::retrieval_queries, WorkflowIntent};
use crate::discovery_store::{DiscoveryAuthorization, DiscoveryStore, RetrievalReceipt};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Serialize, Deserialize)]
pub struct RequirementRetrieval {
    pub requirement_id: String,
    pub query: String,
    pub retrieval: RetrievalReceipt,
    /// IDs not admitted to the shared selector packet. Never hidden in Ready.
    pub omitted_candidate_ids: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct IntentRetrieval {
    pub requirements: Vec<RequirementRetrieval>,
    pub selector: RetrievalReceipt,
}

pub async fn retrieve(
    store: &DiscoveryStore,
    generation: &str,
    workflow: &WorkflowIntent,
    allowed: &DiscoveryAuthorization,
    selector_budget: usize,
) -> Result<IntentRetrieval> {
    let queries = retrieval_queries(workflow)?;
    ensure!(
        (1..=128).contains(&selector_budget),
        "selector budget outside supported range"
    );
    ensure!(
        queries.len() <= selector_budget,
        "selector budget cannot allocate one seat per requirement"
    );
    let mut requirements = Vec::new();
    for (requirement_id, query) in queries {
        let retrieval = store.retrieve(generation, &query, allowed).await?;
        requirements.push(RequirementRetrieval {
            requirement_id,
            query,
            retrieval,
            omitted_candidate_ids: Vec::new(),
        });
    }
    fuse(generation, requirements, selector_budget)
}

/// Round-robin admission avoids allowing the first requirement to occupy the
/// entire packet. Full per-need receipts retain channel and budget losses.
pub fn fuse(
    generation: &str,
    mut requirements: Vec<RequirementRetrieval>,
    budget: usize,
) -> Result<IntentRetrieval> {
    ensure!(
        (1..=128).contains(&budget) && requirements.len() <= budget,
        "invalid selector budget"
    );
    let mut requirement_ids = BTreeSet::new();
    for r in &requirements {
        ensure!(
            r.retrieval.generation == generation
                && !r.requirement_id.trim().is_empty()
                && requirement_ids.insert(&r.requirement_id),
            "mixed generation or duplicate requirement"
        );
    }
    let mut identities = BTreeMap::new();
    for requirement in &requirements {
        for candidate in &requirement.retrieval.candidates {
            let identity = (&candidate.reference, &candidate.document);
            let encoded = serde_json::to_vec(&identity)?;
            if let Some(previous) = identities.insert(&candidate.id, encoded.clone()) {
                ensure!(
                    previous == encoded,
                    "candidate identity has conflicting content"
                );
            }
        }
    }
    let mut candidates = Vec::new();
    let mut admitted = BTreeSet::new();
    let depth = requirements
        .iter()
        .map(|r| r.retrieval.candidates.len())
        .max()
        .unwrap_or(0);
    for index in 0..depth {
        for r in &requirements {
            if let Some(c) = r.retrieval.candidates.get(index) {
                if candidates.len() < budget && admitted.insert(c.id.clone()) {
                    candidates.push(c.clone());
                }
            }
        }
    }
    for r in &mut requirements {
        r.omitted_candidate_ids = r
            .retrieval
            .candidates
            .iter()
            .filter(|c| !admitted.contains(&c.id))
            .map(|c| c.id.clone())
            .collect();
        for c in &mut candidates {
            if r.retrieval
                .candidates
                .iter()
                .any(|offered| offered.id == c.id)
            {
                c.admissions
                    .insert(format!("requirement:{}", r.requirement_id));
            }
        }
    }
    let selector = RetrievalReceipt {
        generation: generation.into(),
        candidates,
        lexical_count: requirements.iter().map(|r| r.retrieval.lexical_count).sum(),
        vector_count: requirements.iter().map(|r| r.retrieval.vector_count).sum(),
        lexical_truncated: requirements.iter().any(|r| r.retrieval.lexical_truncated),
        vector_truncated: requirements.iter().any(|r| r.retrieval.vector_truncated),
        fusion_truncated: requirements
            .iter()
            .map(|r| r.retrieval.fusion_truncated + r.omitted_candidate_ids.len())
            .sum(),
        relation_truncated: requirements
            .iter()
            .map(|r| r.retrieval.relation_truncated)
            .sum(),
    };
    Ok(IntentRetrieval {
        requirements,
        selector,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery_store::RetrievedCapability;
    use plasm_core::{catalog_discovery::CapabilityDocument, prerequisites::CapabilityRef};

    fn need(id: usize, offered: &[u8]) -> RequirementRetrieval {
        RequirementRetrieval {
            requirement_id: format!("r{id}"),
            query: "abstract operation".into(),
            omitted_candidate_ids: vec![],
            retrieval: RetrievalReceipt {
                generation: "fixture".into(),
                lexical_count: offered.len(),
                vector_count: 0,
                lexical_truncated: false,
                vector_truncated: false,
                fusion_truncated: 0,
                relation_truncated: 0,
                candidates: offered
                    .iter()
                    .map(|n| RetrievedCapability {
                        id: format!("revision/op{n}"),
                        reference: CapabilityRef {
                            catalog: "fixture".into(),
                            capability: format!("op{n}"),
                        },
                        document: CapabilityDocument {
                            capability: format!("op{n}"),
                            entity: "Record".into(),
                            text: "Abstract operation".into(),
                            text_hash: "fixture".into(),
                            related_entities: vec![],
                        },
                        admissions: BTreeSet::new(),
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn conflicting_evidence_and_generation_cannot_be_fused() {
        let a = need(0, &[1]);
        let mut b = need(1, &[1]);
        b.retrieval.candidates[0].document.text = "Different operation".into();
        assert!(fuse("fixture", vec![a, b], 2).is_err());
        assert!(fuse("other", vec![need(0, &[1])], 1).is_err());
        assert!(fuse("fixture", vec![need(0, &[1]), need(0, &[2])], 2).is_err());
    }

    #[test]
    fn minority_need_receives_a_seat_and_losses_are_visible() {
        let result = fuse("fixture", vec![need(0, &[1, 2, 3]), need(1, &[4])], 2).unwrap();
        assert_eq!(
            result
                .selector
                .candidates
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            vec!["revision/op1", "revision/op4"]
        );
        assert_eq!(result.requirements[0].omitted_candidate_ids.len(), 2);
        assert_eq!(result.selector.fusion_truncated, 2);
    }

    proptest::proptest! {
        #[test]
        fn fusion_never_invents_or_silently_drops_candidates(
            lists in proptest::collection::vec(proptest::collection::vec(0u8..20,0..12),0..8),
            budget in 8usize..25,
        ) {
            let result = fuse("fixture",lists.iter().enumerate().map(|(i,l)|need(i,l)).collect(),budget).unwrap();
            let published: BTreeSet<_> = result.selector.candidates.iter().map(|c|c.id.clone()).collect();
            let offered: BTreeSet<_> = lists.iter().flatten().map(|n|format!("revision/op{n}")).collect();
            proptest::prop_assert!(published.is_subset(&offered));
            proptest::prop_assert_eq!(published.len(),result.selector.candidates.len());
            proptest::prop_assert_eq!(published.len(),offered.len().min(budget));
            for need in result.requirements {
                for candidate in need.retrieval.candidates {
                    proptest::prop_assert!(published.contains(&candidate.id) || need.omitted_candidate_ids.contains(&candidate.id));
                    if published.contains(&candidate.id) {
                        let row = result.selector.candidates.iter().find(|c|c.id==candidate.id).unwrap();
                        let admission = format!("requirement:{}",need.requirement_id);
                        proptest::prop_assert!(row.admissions.contains(&admission));
                    }
                }
            }
        }
    }
}
