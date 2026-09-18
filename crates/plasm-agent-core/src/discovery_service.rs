//! Staged workflow intent, requirement retrieval, and grounded capability coverage.

#[path = "discovery_intent_pipeline.rs"]
mod intent_pipeline;
#[path = "discovery_selector_contract.rs"]
pub mod selector_contract;

use anyhow::{bail, Context, Result};
use plasm_core::prerequisites::{
    prerequisite_closure_with_source_authorization, CapabilityRef, PrerequisiteClosure,
};
use serde::{Deserialize, Serialize};

use crate::discovery_selection::{self as selection, validate_selection};
use crate::discovery_store::{
    DiscoveryAuthorization, DiscoverySessionPin, DiscoveryStore, RetrievalReceipt,
};

pub use crate::discovery_recovery::{CatalogAppDescription, DiscoveryRecovery, RECOVERY_GUIDANCE};
pub use selection::{
    CapabilitySelection, RequirementAssessment, RequirementCoverage, SelectionStatus,
    UnsupportedWork,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct RoutingReceipt {
    pub authorization: DiscoveryAuthorization,
    pub intent_evidence: Option<intent_pipeline::IntentEvidence>,
    pub intent_analysis: String,
    pub intent: String,
    pub pin_id: String,
    pub retrieval: RetrievalReceipt,
    pub selection: CapabilitySelection,
    pub closure: Option<PrerequisiteClosure>,
    /// Present when selection is insufficient: unresolved clauses + catalog descriptions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<DiscoveryRecovery>,
}

pub struct RouteTurn<'a> {
    pub new_generation: &'a str,
    pub intent: &'a str,
    pub user_requests: &'a [String],
    pub logical_session: Option<&'a str>,
    pub allowed: &'a DiscoveryAuthorization,
    pub exposed: &'a [CapabilityRef],
    pub expires_at: std::time::SystemTime,
}

pub struct DiscoveryService {
    store: DiscoveryStore,
    client: reqwest::Client,
    api_key: String,
    model: String,
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
                .filter(|v| !v.is_empty())
                .map(std::path::PathBuf::from),
            store,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()?,
            api_key,
            model: std::env::var("PLASM_DISCOVERY_AUTO_SEED_MODEL")
                .unwrap_or_else(|_| "openai/gpt-4.1-mini".into()),
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
        let mut receipt = self
            .route(
                &generation,
                request.intent,
                request.user_requests,
                request.allowed,
                request.exposed,
            )
            .await?;
        receipt.pin_id = pin_id;
        Ok(receipt)
    }

    pub async fn route(
        &self,
        generation: &str,
        intent: &str,
        user_requests: &[String],
        allowed: &DiscoveryAuthorization,
        exposed: &[CapabilityRef],
    ) -> Result<RoutingReceipt> {
        let intent_evidence = self.interpret_intent(intent, user_requests).await?;
        let mut gathered = crate::workflow_intent::retrieval::retrieve(
            &self.store,
            generation,
            &intent_evidence.workflow,
            allowed,
            64,
        )
        .await?;
        // Operational focus is evidence for retrieval, never a new user instruction.
        if !intent.trim().is_empty() {
            gathered
                .requirements
                .push(crate::workflow_intent::retrieval::RequirementRetrieval {
                    requirement_id: "focus".into(),
                    query: intent.into(),
                    retrieval: self.store.retrieve(generation, intent, allowed).await?,
                    omitted_candidate_ids: Vec::new(),
                });
            gathered =
                crate::workflow_intent::retrieval::fuse(generation, gathered.requirements, 64)?;
        }
        self.store
            .include_exposed(&mut gathered.selector, exposed, allowed)
            .await?;
        let mut intent_evidence = intent_evidence;
        let issued = crate::workflow_intent::contract::assessment_request(
            &self.model,
            &intent_evidence.workflow,
            &gathered.selector,
            intent,
        )?;
        let (key, raw) = self.intent_response(&issued.body).await?;
        let assessment = crate::workflow_intent::contract::decode_assessment(
            &intent_evidence.workflow,
            &gathered.selector,
            &issued,
            &raw,
        )?;
        self.store.store_selector_envelope(&key, &raw).await?;
        let selection =
            intent_pipeline::selection(&intent_evidence.workflow, &assessment, &gathered.selector)?;
        let business = validate_selection(&selection, &gathered.selector)?;
        intent_evidence.assessment = Some(assessment);
        let intent_analysis = intent_evidence.render()?;
        intent_evidence.requirements = gathered.requirements;
        let retrieval = gathered.selector;
        let needs_catalogs = !business.is_empty()
            || !exposed.is_empty()
            || selection.status == SelectionStatus::Insufficient;
        let loaded = if needs_catalogs {
            Some(self.store.load_generation(generation).await?)
        } else {
            None
        };
        let closure = if !business.is_empty() || !exposed.is_empty() {
            let (catalogs, _compiled_catalogs, bindings) = loaded
                .as_ref()
                .expect("catalogs required for prerequisite closure");
            let references = catalogs.iter().map(|(id, cgs)| (id.clone(), cgs)).collect();
            Some(
                prerequisite_closure_with_source_authorization(
                    &references,
                    &bindings,
                    &business,
                    &allowed.catalogs,
                    |reference| allowed.permits(reference),
                )
                .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        validate_closure_authorization(closure.as_ref(), allowed)?;
        let recovery = if selection.status == SelectionStatus::Insufficient {
            let (catalogs, _, _) = loaded
                .as_ref()
                .expect("catalogs required for insufficient recovery");
            Some(DiscoveryRecovery::from_insufficient(
                &selection.requirement_coverage,
                catalogs,
                allowed.catalogs.iter(),
            ))
        } else {
            None
        };
        Ok(RoutingReceipt {
            authorization: allowed.clone(),
            intent_evidence: Some(intent_evidence),
            intent_analysis,
            intent: intent.to_owned(),
            pin_id: String::new(),
            retrieval,
            selection,
            closure,
            recovery,
        })
    }
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
            .any(|cap| !allowed.permits(cap))
        {
            bail!("declared prerequisite closure contains an unauthorized capability");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery_store::RetrievedCapability;
    use plasm_core::catalog_discovery::CapabilityDocument;
    use std::collections::BTreeSet;
    pub(super) fn receipt() -> RetrievalReceipt {
        RetrievalReceipt {
            generation: "generation".into(),
            lexical_count: 4,
            vector_count: 4,
            lexical_truncated: false,
            vector_truncated: false,
            fusion_truncated: 0,
            relation_truncated: 0,
            candidates: (0..4)
                .map(|n| RetrievedCapability {
                    id: format!("cap-{n}"),
                    reference: CapabilityRef {
                        catalog: "matrix".into(),
                        capability: format!("operation-{n}"),
                    },
                    document: CapabilityDocument {
                        capability: format!("operation-{n}"),
                        entity: format!("Entity{n}"),
                        text: "Independent operation".into(),
                        text_hash: String::new(),
                        related_entities: vec![],
                    },
                    admissions: BTreeSet::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn operational_recovery_is_required_and_selects_new_or_exposed_producer() {
        use crate::workflow_intent::{
            contract, IntentScope, InterpretationDraft, RequirementDraft, RequirementKind,
            WorkflowIntent,
        };
        use serde_json::json;
        let mut workflow = WorkflowIntent::open(
            IntentScope::Workflow,
            "u".into(),
            "Update eligible records only".into(),
        )
        .unwrap();
        workflow
            .interpret(
                1,
                0,
                InterpretationDraft {
                    conditionals: vec![],
                    dispositions: Default::default(),
                    requirements: vec![RequirementDraft {
                        uncertainty: None,
                        kind: RequirementKind::Effect,
                        statement: "Update eligible records only".into(),
                        source_turn_ids: vec!["u0".into()],
                    }],
                    no_requirements_reason: None,
                },
            )
            .unwrap();
        workflow.set_links(1, 1, vec![]).unwrap();
        let original = serde_json::to_value(&workflow).unwrap();
        let focus = "Update failed because its credential expired. Find a credential renewal operation; retain the eligible-record constraint.";
        for exposed in [false, true] {
            let mut receipt = receipt();
            receipt.candidates[0].document.text =
                "Update eligible records using a valid credential".into();
            receipt.candidates[1].document.text = "Renew an expired credential".into();
            if exposed {
                receipt.candidates[1]
                    .admissions
                    .insert("already_exposed".into());
            }
            let issued =
                contract::assessment_request("fixture", &workflow, &receipt, focus).unwrap();
            let body: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
            let input: serde_json::Value =
                serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            assert_eq!(
                input["operational_goal"],
                json!({"id":"f0","statement":focus})
            );
            assert_eq!(input["workflow"], original);
            let coverage = json!([
                {"requirement":"r0","assessment":{"status":"supported","capability_ids":["c0"],"explanation":"Consumer"}},
                {"requirement":"f0","assessment":{"status":"supported","capability_ids":["c1"],"explanation":"Renewal producer"}}
            ]);
            let envelope = |coverage| {
                json!({"choices":[{"finish_reason":"stop","message":{"content":json!({"requirement_coverage":coverage}).to_string()}}]}).to_string()
            };
            assert!(contract::decode_assessment(
                &workflow,
                &receipt,
                &issued,
                &envelope(json!([coverage[0]]))
            )
            .is_err());
            let decoded = contract::decode_assessment(
                &workflow,
                &receipt,
                &issued,
                &envelope(coverage.clone()),
            )
            .unwrap();
            let selected = intent_pipeline::selection(&workflow, &decoded, &receipt).unwrap();
            assert_eq!(
                selected
                    .additional_capability_ids
                    .contains(&"cap-1".to_owned()),
                !exposed
            );
            let business = validate_selection(&selected, &receipt).unwrap();
            assert_eq!(
                business.iter().any(|c| c.capability == "operation-1"),
                !exposed
            );
            assert!(selected
                .requirement_coverage
                .iter()
                .any(|entry| entry.requirement.starts_with("f0:")));
            let mut missing = coverage.clone();
            missing[1]["assessment"] = json!({"status":"unresolved","capability_ids":[],"explanation":"No allowed renewal operation"});
            let decoded =
                contract::decode_assessment(&workflow, &receipt, &issued, &envelope(missing))
                    .unwrap();
            assert_eq!(
                intent_pipeline::selection(&workflow, &decoded, &receipt)
                    .unwrap()
                    .status,
                SelectionStatus::Insufficient
            );
            let mut tampered = serde_json::to_value(&issued).unwrap();
            tampered["operational_goal"] = json!("Change all records");
            let tampered = serde_json::from_value(tampered).unwrap();
            assert!(contract::decode_assessment(
                &workflow,
                &receipt,
                &tampered,
                &envelope(coverage.clone())
            )
            .is_err());
            let other = contract::assessment_request(
                "fixture",
                &workflow,
                &receipt,
                "Inspect credential validity",
            )
            .unwrap();
            assert_ne!(issued.body, other.body); // Provider cache keys include operational focus.
            assert_eq!(serde_json::to_value(&workflow).unwrap(), original);
        }
    }

    #[test]
    fn denied_prerequisite_prevents_ready_exposure() {
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

    #[test]
    fn sufficiency_schema_has_no_conversational_decision() {
        let body = selector_contract::request("model", "intent", &[], &receipt()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let schema = &value["response_format"]["json_schema"]["schema"];
        assert_eq!(*schema, selector_contract::schema(&receipt()));
        assert!(schema["properties"].get("requirements").is_none());
        assert_eq!(schema["properties"].as_object().unwrap().len(), 1);
    }

    #[test]
    fn selector_request_pins_temperature_and_seed() {
        let body = selector_contract::request("model", "inspect records", &[], &receipt()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["temperature"], 0);
        assert_eq!(value["seed"], selector_contract::SELECTOR_SEED);
        assert_eq!(value["provider"]["require_parameters"], true);
    }

    #[test]
    fn selector_request_is_byte_identical_for_permuted_exposed() {
        let exposed_a = [
            CapabilityRef {
                catalog: "zeta".into(),
                capability: "write".into(),
            },
            CapabilityRef {
                catalog: "alpha".into(),
                capability: "read".into(),
            },
        ];
        let mut exposed_b = exposed_a.clone();
        exposed_b.reverse();
        let left =
            selector_contract::request("model", "inspect records", &exposed_a, &receipt()).unwrap();
        let right =
            selector_contract::request("model", "inspect records", &exposed_b, &receipt()).unwrap();
        assert_eq!(left, right);
        assert_eq!(
            selector_contract::request_cache_key(&left),
            selector_contract::request_cache_key(&right)
        );
        let user: serde_json::Value = serde_json::from_str(
            serde_json::from_str::<serde_json::Value>(&left).unwrap()["messages"][1]["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            user["already_exposed"],
            serde_json::json!([
                {"catalog":"alpha","capability":"read"},
                {"catalog":"zeta","capability":"write"}
            ])
        );
    }

    #[test]
    fn selector_request_is_byte_identical_for_permuted_candidates() {
        let receipt_a = receipt();
        let mut receipt_b = receipt();
        receipt_b.candidates.reverse();
        assert_ne!(
            receipt_a.candidates.first().map(|c| c.id.as_str()),
            receipt_b.candidates.first().map(|c| c.id.as_str()),
            "precondition: permutation must change retrieval order"
        );
        let left = selector_contract::request("model", "inspect records", &[], &receipt_a).unwrap();
        let right =
            selector_contract::request("model", "inspect records", &[], &receipt_b).unwrap();
        assert_eq!(left, right);
        assert_eq!(
            selector_contract::request_cache_key(&left),
            selector_contract::request_cache_key(&right)
        );
        let user: serde_json::Value = serde_json::from_str(
            serde_json::from_str::<serde_json::Value>(&left).unwrap()["messages"][1]["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let ids: Vec<&str> = user["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["c0", "c1", "c2", "c3"]);
    }

    #[test]
    fn cached_envelope_round_trips_the_same_selection() {
        let receipt = receipt();
        let selection = CapabilitySelection::from_coverage(
            vec![crate::discovery_selection::RequirementCoverage {
                requirement: "inspect records".into(),
                assessment: RequirementAssessment::Supported {
                    supported_by: vec!["cap-0".into(), "cap-1".into()],
                },
            }],
            &receipt,
        )
        .unwrap();
        let envelope = selector_contract::selection_envelope(&selection, &receipt).unwrap();
        let raw = selector_contract::wrap_cached_envelope(&envelope);
        let (cached, business) = selector_contract::decode(&raw, &receipt).unwrap();
        assert_eq!(
            cached.additional_capability_ids,
            selection.additional_capability_ids
        );
        assert_eq!(business.len(), 2);
        assert_eq!(
            selector_contract::request_cache_key("body-a"),
            plasm_core::catalog_discovery::content_hash(b"body-a")
        );
    }
}

#[cfg(test)]
mod intent_selection_regression {
    use super::*;
    use crate::workflow_intent::contract;
    use crate::workflow_intent::*;
    use serde_json::json;
    proptest::proptest! {
        #[test]
        fn workflow_alias_selection_preserves_wire_identity(order in proptest::collection::vec(proptest::prelude::any::<u64>(), 4)) {
            let mut receipt = super::tests::receipt();
            receipt.candidates.sort_by_key(|c| order[c.id.strip_prefix("cap-").unwrap().parse::<usize>().unwrap()]);
            let mut workflow = WorkflowIntent::open(IntentScope::Workflow, "request".into(), "Read eligible records".into()).unwrap();
            workflow.interpret(1, 0, InterpretationDraft {
        conditionals: vec![],
                dispositions: Default::default(),
                requirements: vec![RequirementDraft { uncertainty: None, kind: RequirementKind::InformationNeed,
                    statement: "Read eligible records".into(), source_turn_ids: vec!["u0".into()] }],
                no_requirements_reason: None,
            }).unwrap();
            workflow.set_links(1, 1, vec![]).unwrap();
            let issued = contract::assessment_request("fixture", &workflow, &receipt, "").unwrap();
            let request: serde_json::Value = serde_json::from_str(&issued.body).unwrap();
            let body: serde_json::Value = serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
            for candidate in body["candidates"].as_array().unwrap() {
                let raw = selector_contract::wrap_cached_envelope(&json!({"requirement_coverage":[{
                    "requirement":"r0", "assessment":{"status":"supported","capability_ids":[candidate["id"]],"explanation":"Provides the records"}
                }]}).to_string());
                let assessment = contract::decode_assessment(&workflow, &receipt, &issued, &raw).unwrap();
                let selection = intent_pipeline::selection(&workflow, &assessment, &receipt).unwrap();
                let refs = crate::discovery_selection::validate_selection(&selection, &receipt).unwrap();
                proptest::prop_assert_eq!(refs.len(), 1);
                proptest::prop_assert_eq!(&refs[0].capability, candidate["reference"]["capability"].as_str().unwrap());
            }
        }
    }
}
