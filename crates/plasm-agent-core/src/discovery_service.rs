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
    pub authorization: DiscoveryAuthorization,
    #[serde(default)]
    pub intent_analysis: String,
    pub intent: String,
    pub pin_id: String,
    pub retrieval: RetrievalReceipt,
    pub matching: CapabilityMatchReceipt,
    pub input_source_projection: Vec<InputSourceCandidate>,
    pub input_source_matching: InputSourceMatchReceipt,
    pub closure: Option<PrerequisiteClosure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<DiscoveryRecovery>,
}

pub struct RouteTurn<'a> {
    pub new_generation: &'a str,
    pub intent: &'a str,
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
        let mut receipt = self
            .route(
                &generation,
                request.intent,
                request.effect_slots,
                request.allowed,
                request.exposed,
            )
            .await?;
        receipt.pin_id = pin_id;
        Ok(receipt)
    }

    /// Deterministic retrieval bounds the candidate universe; Jev independently
    /// matches every card against every explicit affirmative effect slot. No
    /// closure is published unless the host proves every slot has a match.
    pub async fn route(
        &self,
        generation: &str,
        intent: &str,
        effect_slots: &[String],
        allowed: &DiscoveryAuthorization,
        exposed: &[CapabilityRef],
    ) -> Result<RoutingReceipt> {
        let mut retrieval = self.store.retrieve(generation, intent, allowed).await?;
        self.store
            .include_exposed(&mut retrieval, exposed, allowed)
            .await?;
        let slots = effect_slots
            .iter()
            .enumerate()
            .map(|(index, statement)| matcher::EffectSlot {
                id: format!("s{index}"),
                statement: statement.clone(),
            })
            .collect::<Vec<_>>();
        matcher::validate_slots(&slots)?;
        let batches = matcher::issue_batches(&self.match_model, intent, &slots, &retrieval)?;
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
        let business = if matching.complete {
            matched_capabilities(&matching, &retrieval)?
        } else {
            Vec::new()
        };
        let needs_catalogs = !matching.complete || !business.is_empty() || !exposed.is_empty();
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
                intent,
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
            let matched = matcher::finish_input_sources(answers, &projection)?;
            (projection, matched)
        };
        let closure = if matching.complete && (!business.is_empty() || !exposed.is_empty()) {
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
        let recovery = if !matching.complete {
            let (catalogs, _, _) = loaded
                .as_ref()
                .expect("catalogs required for unmatched recovery");
            Some(DiscoveryRecovery::from_unmatched(
                &matching,
                catalogs,
                allowed.catalogs.iter(),
            ))
        } else {
            None
        };
        Ok(RoutingReceipt {
            authorization: allowed.clone(),
            intent_analysis:
                "**Capability selection** (deterministic retrieval; Jev classifies each card against every explicit affirmative effect slot; the host requires complete slot coverage before CGS input projection and closure):"
                    .to_owned(),
            intent: intent.to_owned(),
            pin_id: String::new(),
            retrieval,
            matching,
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
                "contract": "jev-two-stage-capability-routing-v3",
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

fn matched_capabilities(
    matching: &CapabilityMatchReceipt,
    retrieval: &RetrievalReceipt,
) -> Result<Vec<CapabilityRef>> {
    let ids: std::collections::BTreeSet<_> = matching
        .matches
        .iter()
        .filter(|matched| {
            matches!(
                matched.choice,
                crate::discovery_matcher::MatchChoice::DirectMatch
            )
        })
        .map(|matched| matched.capability_id.as_str())
        .collect();
    ids.into_iter()
        .map(|id| {
            retrieval
                .candidates
                .iter()
                .find(|candidate| candidate.id == id)
                .map(|candidate| candidate.reference.clone())
                .context("matched capability disappeared from retrieval receipt")
        })
        .collect()
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
    use std::collections::BTreeSet;

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
