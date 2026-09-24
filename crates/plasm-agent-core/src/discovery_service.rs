//! Direct, bounded capability discovery: deterministic recall followed by Jev matching.

use anyhow::{bail, ensure, Context, Result};
use plasm_core::prerequisites::{prerequisite_closure, CapabilityRef, PrerequisiteClosure};
use serde::{Deserialize, Serialize};

use crate::discovery_matcher::{self as matcher, CapabilityMatchReceipt};
use crate::discovery_store::{
    DiscoveryAuthorization, DiscoverySessionPin, DiscoveryStore, RetrievalReceipt,
};

pub use crate::discovery_recovery::{CatalogAppDescription, DiscoveryRecovery, RECOVERY_GUIDANCE};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutingReceipt {
    pub intent_provenance: crate::intent_provenance::IntentProvenance,
    pub authorization: DiscoveryAuthorization,
    #[serde(default)]
    pub intent_analysis: String,
    pub intent: String,
    pub pin_id: String,
    pub retrieval: RetrievalReceipt,
    pub matching: CapabilityMatchReceipt,
    pub closure: Option<PrerequisiteClosure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<DiscoveryRecovery>,
}

pub struct RouteTurn<'a> {
    pub new_generation: &'a str,
    pub intent_provenance: &'a crate::intent_provenance::IntentProvenance,
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
    #[cfg(test)]
    pub(crate) fn cached_test_service(store: DiscoveryStore) -> Self {
        Self {
            store,
            client: reqwest::Client::builder()
                .proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap())
                .timeout(std::time::Duration::from_secs(1))
                .build()
                .unwrap(),
            api_key: "no-provider-calls-permitted".into(),
            match_model: matcher::JEV_MODEL.into(),
            rejection_dir: None,
        }
    }
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
                request.intent_provenance,
                request.allowed,
                request.exposed,
            )
            .await?;
        receipt.pin_id = pin_id;
        self.store
            .commit_discovery_progress(
                &receipt.pin_id,
                request.intent_provenance,
                request.logical_session.is_some(),
            )
            .await?;
        Ok(receipt)
    }

    /// Deterministic retrieval bounds the candidate universe; Jev independently
    /// judges relevance over typed candidates. Selection never certifies task completion.
    pub async fn route(
        &self,
        generation: &str,
        provenance: &crate::intent_provenance::IntentProvenance,
        allowed: &DiscoveryAuthorization,
        exposed: &[CapabilityRef],
    ) -> Result<RoutingReceipt> {
        ensure!(
            exposed.iter().all(|r| allowed.permits(r)),
            "exposed capability is no longer authorized"
        );
        let retrieval = self
            .store
            .retrieve(generation, provenance.current(), allowed)
            .await?;
        let (catalogs, bindings) = self
            .store
            .load_discovery_generation(generation, allowed)
            .await?;
        ensure!(
            exposed.iter().all(|r| catalogs
                .get(&r.catalog)
                .is_some_and(|cgs| cgs.capabilities.contains_key(r.capability.as_str()))),
            "exposed capability is absent from pinned generation"
        );
        retrieval.validate(allowed)?;
        let matching = matcher::judge_candidates(
            &self.match_model,
            provenance,
            &retrieval,
            |issued| async move {
                if let Some(raw) = self
                    .store
                    .cached_selector_envelope(issued.cache_key())
                    .await?
                {
                    return Ok(raw);
                }
                let raw = self.jev_response(issued.body()).await?;
                matcher::decode_batch(&issued, &raw)?;
                self.store
                    .store_selector_envelope(issued.cache_key(), &raw)
                    .await?;
                Ok(raw)
            },
        )
        .await?;
        let business = matching.selected(&retrieval);
        let closure = if !business.is_empty() || !exposed.is_empty() {
            let references = catalogs.iter().map(|(id, cgs)| (id.clone(), cgs)).collect();
            Some(
                prerequisite_closure(&references, &bindings, &business, &allowed.catalogs)
                    .map_err(anyhow::Error::msg)?,
            )
        } else {
            None
        };
        validate_closure_authorization(closure.as_ref(), allowed)?;
        let recovery = if business.is_empty() {
            Some(DiscoveryRecovery::from_unmatched(
                &matching,
                &retrieval,
                &catalogs,
                allowed.catalogs.iter(),
            ))
        } else {
            None
        };
        Ok(RoutingReceipt {
            intent_provenance: provenance.clone(),
            authorization: allowed.clone(),
            intent_analysis:
                "**Capability relevance** (authorized bounded channel union and paged judgment; typed evidence; declared prerequisite closure):"
                    .to_owned(),
            intent: provenance.current().to_owned(),
            pin_id: String::new(),
            retrieval,
            matching,
            closure,
            recovery,
        })
    }

    async fn jev_response(&self, body: &str) -> Result<String> {
        crate::decision_transport::request_decision(
            &self.client,
            "https://openrouter.ai/api/alpha/decisions",
            &self.api_key,
            body,
            crate::decision_transport::DecisionRetryPolicy::default(),
            |attempt| {
                if let Some(directory) = &self.rejection_dir {
                    let mut record = serde_json::to_value(attempt)?;
                    record["contract"] = "jev-capability-relevance-v2".into();
                    record["request_body"] = body.into();
                    save_rejection(directory, &record)?;
                }
                Ok(())
            },
        )
        .await
    }
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
