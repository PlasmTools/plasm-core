//! One grounded capability selector followed by deterministic prerequisite closure.

#[path = "discovery_selector_contract.rs"]
pub mod selector_contract;

use anyhow::{bail, Context, Result};
use plasm_core::prerequisites::{prerequisite_closure, CapabilityRef, PrerequisiteClosure};
use serde::{Deserialize, Serialize};

use crate::discovery_store::{
    DiscoveryAuthorization, DiscoverySessionPin, DiscoveryStore, RetrievalReceipt,
};

#[path = "discovery_selection.rs"]
mod selection;
use selection::validate_selection;
pub use selection::{CapabilitySelection, SelectionStatus, UnsupportedWork};

#[derive(Debug, Serialize, Deserialize)]
pub struct RoutingReceipt {
    pub authorization: DiscoveryAuthorization,
    pub intent: String,
    pub pin_id: String,
    pub retrieval: RetrievalReceipt,
    pub selection: CapabilitySelection,
    pub closure: Option<PrerequisiteClosure>,
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
        let decoded = if status.is_success() {
            selector_contract::decode(&raw, intent, &retrieval)
        } else {
            Err(anyhow::anyhow!("capability selector failed: HTTP {status}"))
        };
        let (selection, business) = match decoded {
            Ok(valid) => valid,
            Err(error) => {
                if let Some(directory) = &self.rejection_dir {
                    let directory = directory.clone();
                    let record = serde_json::json!({
                        "contract_version": 6, "model": self.model, "generation": generation,
                        "request_body": request_body,
                        "input": input, "http_status": status.as_u16(),
                        "instructions": selector_contract::INSTRUCTIONS, "schema": selector_contract::schema(),
                        "temperature": 0, "retrieval": retrieval,
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
        let closure = if !business.is_empty() || !exposed.is_empty() {
            let (catalogs, _compiled_catalogs, bindings) =
                self.store.load_generation(generation).await?;
            let references = catalogs.iter().map(|(id, cgs)| (id.clone(), cgs)).collect();
            Some(
                prerequisite_closure(&references, &bindings, &business, &allowed.catalogs)
                    .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        validate_closure_authorization(closure.as_ref(), allowed)?;
        Ok(RoutingReceipt {
            authorization: allowed.clone(),
            intent: intent.to_owned(),
            pin_id: String::new(),
            retrieval,
            selection,
            closure,
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
        assert_eq!(*schema, selector_contract::schema());
        assert!(schema["properties"].get("requirements").is_none());
        assert_eq!(schema["properties"].as_object().unwrap().len(), 2);
    }
}
