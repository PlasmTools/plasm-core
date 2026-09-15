//! One grounded capability selector followed by deterministic prerequisite closure.

#[path = "discovery_selector_contract.rs"]
pub mod selector_contract;

use anyhow::{bail, Context, Result};
use plasm_core::prerequisites::{prerequisite_closure, CapabilityRef, PrerequisiteClosure};
use serde::{Deserialize, Serialize};

use crate::discovery_selection::{self as selection, validate_selection};
use crate::discovery_store::{
    DiscoveryAuthorization, DiscoverySessionPin, DiscoveryStore, RetrievalReceipt,
};

pub use crate::discovery_recovery::{CatalogAppDescription, DiscoveryRecovery, RECOVERY_GUIDANCE};
pub use selection::{CapabilitySelection, RequirementCoverage, SelectionStatus, UnsupportedWork};

#[derive(Debug, Serialize, Deserialize)]
pub struct RoutingReceipt {
    pub authorization: DiscoveryAuthorization,
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
                .timeout(std::time::Duration::from_secs(90))
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
        allowed: &DiscoveryAuthorization,
        exposed: &[CapabilityRef],
    ) -> Result<RoutingReceipt> {
        let mut retrieval = self.store.retrieve(generation, intent, allowed).await?;
        self.store
            .include_exposed(&mut retrieval, exposed, allowed)
            .await?;
        let input = serde_json::json!({"intent":intent,"already_exposed":exposed,"candidates":retrieval.candidates});
        let request_body = selector_contract::request(&self.model, intent, exposed, &retrieval)?;
        let cache_key = selector_contract::request_cache_key(&request_body);
        let (raw, status, from_cache) =
            if let Some(envelope) = self.store.cached_selector_envelope(&cache_key).await? {
                (
                    selector_contract::wrap_cached_envelope(&envelope),
                    reqwest::StatusCode::OK,
                    true,
                )
            } else {
                let response = self
                    .client
                    .post("https://openrouter.ai/api/v1/chat/completions")
                    .bearer_auth(&self.api_key)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(request_body.clone())
                    .send()
                    .await
                    .context("capability selector transport failed")?;
                let status = response.status();
                let raw = response
                    .text()
                    .await
                    .context("reading capability selector response")?;
                (raw, status, false)
            };
        let decoded = if status.is_success() {
            selector_contract::decode(&raw, &retrieval)
        } else {
            Err(anyhow::anyhow!("capability selector failed: HTTP {status}"))
        };
        let (selection, business) = match decoded {
            Ok(valid) => {
                if !from_cache {
                    self.store
                        .store_selector_envelope(
                            &cache_key,
                            &selector_contract::selection_envelope(&valid.0)?,
                        )
                        .await?;
                }
                valid
            }
            Err(error) => {
                if let Some(directory) = &self.rejection_dir {
                    let directory = directory.clone();
                    let record = serde_json::json!({
                        "contract_version": 8, "model": self.model, "generation": generation,
                        "request_body": request_body,
                        "input": input, "http_status": status.as_u16(),
                        "instructions": selector_contract::INSTRUCTIONS, "schema": selector_contract::schema(&retrieval),
                        "temperature": 0, "seed": selector_contract::SELECTOR_SEED,
                        "retrieval": retrieval,
                        "raw_response": raw, "error": format!("{error:#}")
                    });
                    let saved = tokio::task::spawn_blocking(move || {
                        selector_contract::save_rejection(&directory, &record)
                    })
                    .await;
                    match saved {
                        Ok(Ok(path)) => {
                            return Err(error.context(format!(
                                "selector rejected; diagnostic {}",
                                path.display()
                            )))
                        }
                        Ok(Err(capture)) => {
                            return Err(error.context(format!(
                                "selector rejection capture failed: {capture:#}"
                            )))
                        }
                        Err(capture) => {
                            return Err(error.context(format!(
                                "selector rejection capture task failed: {capture}"
                            )))
                        }
                    }
                }
                return Err(error);
            }
        };
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
                prerequisite_closure(&references, &bindings, &business, &allowed.catalogs)
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
    fn receipt() -> RetrievalReceipt {
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
        assert_eq!(schema["properties"].as_object().unwrap().len(), 2);
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
        assert_eq!(ids, vec!["cap-0", "cap-1", "cap-2", "cap-3"]);
    }

    #[test]
    fn cached_envelope_round_trips_the_same_selection() {
        let receipt = receipt();
        let envelope = selector_contract::selection_envelope(&CapabilitySelection {
            status: SelectionStatus::Ready,
            additional_capability_ids: vec!["cap-1".into(), "cap-0".into()],
            requirement_coverage: vec![],
        })
        .unwrap();
        let raw = selector_contract::wrap_cached_envelope(&envelope);
        let (selection, business) = selector_contract::decode(&raw, &receipt).unwrap();
        assert_eq!(
            selection.additional_capability_ids,
            vec!["cap-1".to_string(), "cap-0".to_string()]
        );
        assert_eq!(business.len(), 2);
        assert_eq!(
            selector_contract::request_cache_key("body-a"),
            plasm_core::catalog_discovery::content_hash(b"body-a")
        );
    }
}
