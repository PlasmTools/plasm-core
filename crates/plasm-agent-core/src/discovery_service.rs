//! Direct, bounded capability discovery: deterministic recall followed by Jev matching.

use anyhow::{bail, ensure, Context, Result};
use plasm_core::catalog_discovery::{capability_documents, CapabilityDocument};
use plasm_core::prerequisites::{
    prerequisite_closure_with_selected_sources, project_input_source_candidates, CapabilityRef,
    InputSourceCandidate, PrerequisiteClosure,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::discovery_matcher::{self as matcher, CapabilityMatchReceipt, InputSourceMatchReceipt};
use crate::discovery_store::{
    DiscoveryAuthorization, DiscoverySessionPin, DiscoveryStore, RetrievalReceipt,
};

pub use crate::discovery_recovery::{CatalogAppDescription, DiscoveryRecovery, RECOVERY_GUIDANCE};

#[derive(Debug, Serialize, Deserialize)]
pub struct RoutingReceipt {
    pub intent_provenance: crate::intent_provenance::IntentProvenance,
    pub authorization: DiscoveryAuthorization,
    #[serde(default)]
    pub intent_analysis: String,
    pub intent: String,
    pub pin_id: String,
    pub retrieval: RetrievalReceipt,
    pub matching: CapabilityMatchReceipt,
    pub coverage: crate::discovery_coverage::DiscoveryCoverage,
    pub input_source_projection: Vec<InputSourceCandidate>,
    pub input_source_matching: InputSourceMatchReceipt,
    pub closure: Option<PrerequisiteClosure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<DiscoveryRecovery>,
}

pub struct RouteTurn<'a> {
    pub new_generation: &'a str,
    pub intent_provenance: &'a crate::intent_provenance::IntentProvenance,
    pub effect_slots: &'a [String],
    pub logical_session: Option<&'a str>,
    pub allowed: &'a DiscoveryAuthorization,
    pub exposed: &'a [CapabilityRef],
    pub expires_at: std::time::SystemTime,
}

pub struct DiscoveryService {
    store: DiscoveryStore,
    client: reqwest::Client,
    api_key: String,
    match_model: String,
    rejection_dir: Option<std::path::PathBuf>,
}

impl DiscoveryService {
    pub fn from_env(store: DiscoveryStore) -> Result<Self> {
        let api_key = std::env::var("OPENROUTER_API_KEY")
            .context("discovery selector requires OPENROUTER_API_KEY")?;
        if api_key.trim().is_empty() {
            bail!("empty discovery selector key");
        }
        Ok(Self {
            rejection_dir: std::env::var_os("PLASM_DISCOVERY_REJECTION_DIR")
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from),
            store,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()?,
            api_key,
            match_model: std::env::var("PLASM_DISCOVERY_CAPABILITY_MATCH_MODEL")
                .unwrap_or_else(|_| matcher::JEV_MODEL.into()),
        })
    }

    /// Shared new/extend state machine; hosts supply authenticated scope and expiry.
    pub async fn route_turn(&self, request: RouteTurn<'_>) -> Result<RoutingReceipt> {
        let pin_id = request
            .logical_session
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let generation = if request.logical_session.is_some() {
            let generation = self.store.pinned_generation(&pin_id).await?;
            self.store
                .renew_session_pin(
                    &DiscoverySessionPin {
                        pin_id: pin_id.clone(),
                        generation: generation.clone(),
                        authorization: request.allowed.clone(),
                    },
                    request.expires_at.into(),
                )
                .await?;
            generation
        } else {
            self.store
                .pin_generation(&pin_id, request.new_generation, request.expires_at.into())
                .await?;
            request.new_generation.to_owned()
        };
        let coverage = if request.logical_session.is_some() {
            self.store.discovery_coverage(&pin_id).await?
        } else {
            crate::discovery_coverage::DiscoveryCoverage::default()
        };
        let mut receipt = self
            .route(
                &generation,
                request.intent_provenance,
                request.effect_slots,
                request.allowed,
                request.exposed,
                coverage,
            )
            .await?;
        receipt.pin_id = pin_id;
        self.store
            .commit_discovery_progress(
                &receipt.pin_id,
                request.intent_provenance,
                &receipt.coverage,
                request.logical_session.is_some(),
            )
            .await?;
        Ok(receipt)
    }

    /// Deterministic retrieval bounds the candidate universe; Jev independently
    /// matches cards against current effect slots. Matched capabilities
    /// can be exposed independently; coverage never certifies task completion.
    pub async fn route(
        &self,
        generation: &str,
        provenance: &crate::intent_provenance::IntentProvenance,
        effect_slots: &[String],
        allowed: &DiscoveryAuthorization,
        exposed: &[CapabilityRef],
        mut coverage: crate::discovery_coverage::DiscoveryCoverage,
    ) -> Result<RoutingReceipt> {
        let queries = provenance.retrieval_queries(effect_slots)?;
        let mut retrieval = self
            .store
            .retrieve_queries(generation, &queries, allowed)
            .await?;
        self.store
            .include_exposed(&mut retrieval, exposed, allowed)
            .await?;
        let slots = coverage.slots_for_turn(effect_slots)?;
        matcher::validate_slots(&slots)?;
        let batches = matcher::issue_batches(&self.match_model, provenance, &slots, &retrieval)?;
        let mut answers = Vec::new();
        for issued in &batches {
            let raw = if let Some(cached) = self
                .store
                .cached_selector_envelope(&issued.cache_key)
                .await?
            {
                cached
            } else {
                let raw = self.jev_response(&issued.body).await?;
                self.store
                    .store_selector_envelope(&issued.cache_key, &raw)
                    .await?;
                raw
            };
            answers.extend(matcher::decode_batch(issued, &raw)?);
        }
        let matching = matcher::finish(slots, answers, &retrieval)?;
        coverage.observe(&matching, &retrieval)?;
        let business = coverage.matched_capabilities();
        ensure!(
            business.iter().all(|reference| allowed.permits(reference)),
            "previously matched capability is no longer authorized"
        );
        let needs_catalogs =
            coverage.unresolved().next().is_some() || !business.is_empty() || !exposed.is_empty();
        let loaded = if needs_catalogs {
            Some(self.store.load_generation(generation).await?)
        } else {
            None
        };
        let (input_source_projection, input_source_matching) = if business.is_empty() {
            (
                Vec::new(),
                InputSourceMatchReceipt {
                    matches: Vec::new(),
                    selected: Vec::new(),
                },
            )
        } else {
            let (catalogs, _, _) = loaded
                .as_ref()
                .expect("catalogs required for input-source projection");
            let catalog_refs: BTreeMap<_, _> =
                catalogs.iter().map(|(id, cgs)| (id.clone(), cgs)).collect();
            let projection =
                project_input_source_candidates(&catalog_refs, &business, &|reference| {
                    allowed.permits(reference)
                })
                .map_err(anyhow::Error::msg)?;
            let documents = capability_document_index(catalogs)?;
            let batches = matcher::issue_input_source_batches(
                &self.match_model,
                provenance,
                &projection,
                &documents,
            )?;
            let mut answers = Vec::new();
            for issued in &batches {
                let raw = if let Some(cached) = self
                    .store
                    .cached_selector_envelope(&issued.cache_key)
                    .await?
                {
                    cached
                } else {
                    let raw = self.jev_response(&issued.body).await?;
                    self.store
                        .store_selector_envelope(&issued.cache_key, &raw)
                        .await?;
                    raw
                };
                answers.extend(matcher::decode_input_source_batch(issued, &raw)?);
            }
            let matched = matcher::finish_input_sources(answers, &projection, &batches)?;
            (projection, matched)
        };
        let closure = if !business.is_empty() || !exposed.is_empty() {
            let (catalogs, _compiled_catalogs, bindings) = loaded
                .as_ref()
                .expect("catalogs required for prerequisite closure");
            let references = catalogs.iter().map(|(id, cgs)| (id.clone(), cgs)).collect();
            Some(
                prerequisite_closure_with_selected_sources(
                    &references,
                    &bindings,
                    &business,
                    &input_source_matching.selected,
                    &allowed.catalogs,
                    |reference| allowed.permits(reference),
                )
                .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        validate_closure_authorization(closure.as_ref(), allowed)?;
        let recovery = if coverage.unresolved().next().is_some() {
            let (catalogs, _, _) = loaded
                .as_ref()
                .expect("catalogs required for unmatched recovery");
            Some(DiscoveryRecovery::from_unmatched(
                &coverage,
                &matching,
                &retrieval,
                catalogs,
                allowed.catalogs.iter(),
            ))
        } else {
            None
        };
        Ok(RoutingReceipt {
            intent_provenance: provenance.clone(),
            authorization: allowed.clone(),
            intent_analysis:
                "**Capability selection** (deterministic retrieval; Jev classifies each card against every explicit affirmative effect slot; matched capabilities receive CGS input projection and closure; unresolved slots remain explicit):"
                    .to_owned(),
            intent: provenance.current().to_owned(),
            pin_id: String::new(),
            retrieval,
            matching,
            coverage,
            input_source_projection,
            input_source_matching,
            closure,
            recovery,
        })
    }

    async fn jev_response(&self, body: &str) -> Result<String> {
        let response = self
            .client
            .post("https://openrouter.ai/api/alpha/decisions")
            .bearer_auth(&self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_owned())
            .send()
            .await
            .context("Jev Decisions transport")?;
        let status = response.status();
        let raw = response.text().await?;
        if let Some(directory) = &self.rejection_dir {
            let record = serde_json::json!({
                "contract": "jev-two-stage-capability-routing-v4",
                "request_body": body,
                "http_status": status.as_u16(),
                "raw_response": raw,
            });
            save_rejection(directory, &record)?;
        }
        ensure!(status.is_success(), "Jev Decisions HTTP {status}");
        Ok(raw)
    }
}

fn capability_document_index(
    catalogs: &BTreeMap<String, plasm_core::CGS>,
) -> Result<BTreeMap<CapabilityRef, CapabilityDocument>> {
    let mut documents = BTreeMap::new();
    for (catalog, cgs) in catalogs {
        for document in capability_documents(cgs).map_err(anyhow::Error::msg)? {
            let reference = CapabilityRef {
                catalog: catalog.clone(),
                capability: document.capability.clone(),
            };
            ensure!(
                documents.insert(reference, document).is_none(),
                "duplicate capability document"
            );
        }
    }
    Ok(documents)
}

fn save_rejection(directory: &std::path::Path, record: &serde_json::Value) -> Result<()> {
    use std::io::Write;

    std::fs::create_dir_all(directory).context("create discovery diagnostic directory")?;
    let path = directory.join(format!("jev-rejection-{}.json", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).context("create discovery diagnostic")?;
    file.write_all(&serde_json::to_vec_pretty(record)?)?;
    file.sync_all()?;
    Ok(())
}

fn validate_closure_authorization(
    closure: Option<&PrerequisiteClosure>,
    allowed: &DiscoveryAuthorization,
) -> Result<()> {
    if let Some(closure) = closure {
        if closure
            .business
            .iter()
            .chain(&closure.input_sources)
            .chain(&closure.prerequisites)
            .any(|capability| !allowed.permits(capability))
        {
            bail!("declared prerequisite closure contains an unauthorized capability");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::BTreeSet;

    proptest! {
        #[test]
        fn partial_coverage_exposes_only_positive_matches_across_serialization(mask in prop::collection::vec(any::<bool>(), 1..16)) {
            let packet: serde_json::Value = serde_json::from_str(include_str!("../../../fixtures/discovery/partial-routing.json")).unwrap();
            let route: RoutingReceipt = serde_json::from_value(packet["routing"].clone()).unwrap();
            let mut coverage = crate::discovery_coverage::DiscoveryCoverage::default();
            let slots = coverage.slots_for_turn(&(0..mask.len()).map(|i| format!("Requested effect {i}")).collect::<Vec<_>>()).unwrap();
            let answers = slots.iter().zip(&mask).map(|(slot, &direct)| matcher::CapabilityIntentMatch {
                slot_id: slot.id.clone(), capability_id: "matrix/record_read".into(),
                choice: if direct { matcher::MatchChoice::DirectMatch } else { matcher::MatchChoice::DoesNotMatch },
                probabilities: BTreeMap::from([("direct_match".into(), if direct {1.0} else {0.0}), ("does_not_match".into(), if direct {0.0} else {1.0}), ("uncertain".into(), 0.0)]), confidence: 1.0,
            }).collect();
            let matching = matcher::finish(slots, answers, &route.retrieval).unwrap();
            let decoded: CapabilityMatchReceipt = serde_json::from_value(serde_json::to_value(&matching).unwrap()).unwrap();
            prop_assert_eq!(&decoded, &matching);
            prop_assert_eq!(decoded.complete, mask.iter().all(|&bit| bit));
            prop_assert_eq!(decoded.additional_capability_ids.len(), usize::from(mask.iter().any(|&bit| bit)));
            coverage.observe(&decoded, &route.retrieval).unwrap();
            prop_assert_eq!(coverage.matched_capabilities().len(), usize::from(mask.iter().any(|&bit| bit)));
            let wire: crate::discovery_coverage::DiscoveryCoverage = serde_json::from_value(serde_json::to_value(&coverage).unwrap()).unwrap();
            prop_assert_eq!(&wire, &coverage);
            prop_assert_eq!(wire.unresolved().count(), mask.iter().filter(|&&bit| !bit).count());
            let recovery = DiscoveryRecovery::from_unmatched(&wire, &decoded, &route.retrieval, &BTreeMap::new(), Vec::<String>::new());
            prop_assert_eq!(recovery.unmatched_slots.len(), wire.unresolved().count());
        }
    }

    #[test]
    fn partial_routing_wire_preserves_matches_obligations_and_recovery() {
        let packet: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/discovery/partial-routing.json"
        ))
        .unwrap();
        let route: RoutingReceipt = serde_json::from_value(packet["routing"].clone()).unwrap();
        let matching = matcher::finish(
            route.matching.slots.clone(),
            route.matching.matches.clone(),
            &route.retrieval,
        )
        .unwrap();
        assert_eq!(matching, route.matching);
        validate_closure_authorization(route.closure.as_ref(), &route.authorization).unwrap();
        let mut coverage = crate::discovery_coverage::DiscoveryCoverage::default();
        coverage
            .slots_for_turn(
                &matching
                    .slots
                    .iter()
                    .map(|slot| slot.statement.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        coverage.observe(&matching, &route.retrieval).unwrap();
        assert_eq!(coverage, route.coverage);
        assert_eq!(
            coverage.matched_capabilities(),
            route.closure.as_ref().unwrap().business
        );
        let recovery = DiscoveryRecovery::from_unmatched(
            &coverage,
            &matching,
            &route.retrieval,
            &BTreeMap::new(),
            Vec::<String>::new(),
        );
        assert_eq!(
            recovery.unmatched_slots,
            route.recovery.unwrap().unmatched_slots
        );
        assert!(recovery
            .render_unmatched_markdown()
            .contains("matrix/record_read"));
        assert!(recovery.render_unmatched_markdown().contains("rejected"));

        // A later negative judgment cannot erase an earlier positive witness.
        let previous = coverage.clone();
        let slots = coverage
            .slots_for_turn(&["Read selected records".into()])
            .unwrap();
        let matches = slots
            .iter()
            .map(|slot| matcher::CapabilityIntentMatch {
                slot_id: slot.id.clone(),
                capability_id: "matrix/record_read".into(),
                choice: matcher::MatchChoice::DoesNotMatch,
                probabilities: BTreeMap::from([
                    ("direct_match".into(), 0.0),
                    ("does_not_match".into(), 1.0),
                    ("uncertain".into(), 0.0),
                ]),
                confidence: 1.0,
            })
            .collect();
        let rejected = matcher::finish(slots, matches, &route.retrieval).unwrap();
        coverage.observe(&rejected, &route.retrieval).unwrap();
        assert_eq!(coverage.obligations(), previous.obligations());
        let roundtrip: crate::discovery_coverage::DiscoveryCoverage =
            serde_json::from_value(serde_json::to_value(&coverage).unwrap()).unwrap();
        assert_eq!(
            roundtrip
                .unresolved()
                .map(|slot| slot.id.as_str())
                .collect::<Vec<_>>(),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn unauthorized_declared_prerequisite_blocks_exposure() {
        let read = CapabilityRef {
            catalog: "matrix".into(),
            capability: "read".into(),
        };
        let acquire = CapabilityRef {
            catalog: "matrix".into(),
            capability: "acquire".into(),
        };
        let closure = PrerequisiteClosure {
            business: vec![read],
            input_sources: vec![],
            prerequisites: vec![acquire],
            acquisitions: vec![],
            edges: vec![],
        };
        let mut authorization = DiscoveryAuthorization::catalogs(BTreeSet::from(["matrix".into()]));
        authorization
            .capabilities
            .insert("matrix".into(), BTreeSet::from(["read".into()]));
        assert!(validate_closure_authorization(Some(&closure), &authorization).is_err());
    }
}
