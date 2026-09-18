//! Provider orchestration for the shared intent contract. No task-specific rules.
use super::DiscoveryService;
use crate::discovery_selection::{CapabilitySelection, RequirementAssessment, RequirementCoverage};
use crate::discovery_store::RetrievalReceipt;
use crate::workflow_intent::{contract, staged, IntentScope, WorkflowIntent};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Serialize, Deserialize)]
pub struct IntentEvidence {
    pub workflow: WorkflowIntent,
    pub questions: staged::Questions,
    pub assessment: Option<contract::AssessmentResult>,
    pub requirements: Vec<crate::workflow_intent::retrieval::RequirementRetrieval>,
}
impl IntentEvidence {
    pub fn render(&self) -> Result<String> {
        let mut lines=vec!["**Workflow interpretation** (inferred; capability coverage is not execution approval):".to_owned()];
        for r in &self.workflow.linked()?.requirements {
            lines.push(format!("- `{}` {:?}: {}", r.id, r.kind, r.statement));
        }
        if let Some(goal) = self
            .assessment
            .as_ref()
            .and_then(|a| a.operational_goal.as_deref())
        {
            lines.push(format!(
                "- `f0` Operational discovery goal (not a new user instruction): {goal}"
            ));
        }
        for link in self.workflow.linked()?.links.as_deref().unwrap_or(&[]) {
            use crate::workflow_intent::links::RequirementLink;
            let relation = match link {
                RequirementLink::Constrains { .. } => "constrains",
                RequirementLink::WhenTrue { .. } => "when true enables",
                RequirementLink::WhenFalse { .. } => "when false enables",
                RequirementLink::Before { .. } => "before",
            };
            let (source, target) = link.endpoints();
            lines.push(format!("- `{source}` {relation} `{target}`"));
        }
        for d in crate::workflow_intent::decisions::impacts(self.workflow.linked()?) {
            lines.push(format!("- Unresolved `{}` affecting {}: {} Alternatives: {}. Investigate without treating an alternative as settled.",d.decision.id,d.requirement_ids.join(", "),d.decision.question.question,d.decision.question.alternatives.join(" | ")));
        }
        Ok(lines.join("\n"))
    }
}
impl DiscoveryService {
    pub(super) async fn intent_response(&self, body: &str) -> Result<(String, String)> {
        let mut request: serde_json::Value = serde_json::from_str(body)?;
        request["max_tokens"] = json!(16384);
        let effort =
            std::env::var("PLASM_DISCOVERY_REASONING_EFFORT").unwrap_or_else(|_| "none".into());
        ensure!(
            ["none", "low", "medium", "high"].contains(&effort.as_str()),
            "invalid discovery reasoning effort"
        );
        request["reasoning"] = if effort == "none" {
            json!({"enabled":false})
        } else {
            json!({"effort":effort,"enabled":true})
        };
        let body = request.to_string();
        let key =
            super::selector_contract::request_cache_key(&format!("workflow-intent-v2\n{body}"));
        if let Some(raw) = self.store.cached_selector_envelope(&key).await? {
            return Ok((key, raw));
        }
        let response = self
            .client
            .post("https://openrouter.ai/api/v1/chat/completions")
            .bearer_auth(&self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.clone())
            .send()
            .await
            .context("intent provider transport")?;
        let status = response.status();
        let raw = response.text().await?;
        // Capture successful boundaries too: intent failures cannot be diagnosed
        // from the final selector output alone. Never record credentials.
        if let Some(directory) = &self.rejection_dir {
            let record = json!({"contract":"workflow-intent-v2","request_body":body,"http_status":status.as_u16(),"raw_response":raw});
            super::selector_contract::save_rejection(directory, &record)?;
        }
        ensure!(status.is_success(), "intent provider HTTP {status}");
        Ok((key, raw))
    }

    pub(super) async fn interpret_intent(
        &self,
        focus: &str,
        requests: &[String],
    ) -> Result<IntentEvidence> {
        let (scope, turns) = if requests.is_empty() {
            (IntentScope::Focus, vec![focus.to_owned()])
        } else {
            (IntentScope::Workflow, requests.to_vec())
        };
        ensure!(turns.len() <= 64, "too many workflow turns");
        let mut workflow = WorkflowIntent::open(scope, "turn-0".into(), turns[0].clone())?;
        let mut questions = staged::Questions::default();
        // Reconstruct from the immutable user prefix. Validated provider envelopes
        // are cached by exact request; focus changes cannot rewrite that prefix.
        for (index, turn) in turns.iter().enumerate() {
            if index > 0 {
                workflow.append(workflow.revision(), format!("turn-{index}"), turn.clone())?;
            }
            let issued = staged::question_request(&self.model, &workflow, &questions)?;
            let (key, raw) = self.intent_response(&issued.body).await?;
            questions = staged::decode_questions(&workflow, &questions, &issued, &raw)?;
            self.store.store_selector_envelope(&key, &raw).await?;
            let issued = staged::commit_request(&self.model, &workflow, &questions)?;
            let (key, raw) = self.intent_response(&issued.body).await?;
            (workflow, questions) = staged::decode_commit(&workflow, &questions, &issued, &raw)?;
            self.store.store_selector_envelope(&key, &raw).await?;
        }
        let issued = contract::linkage_request(&self.model, &workflow)?;
        let (key, raw) = self.intent_response(&issued.body).await?;
        workflow = contract::decode_linkage(&workflow, &issued, &raw)?;
        self.store.store_selector_envelope(&key, &raw).await?;
        Ok(IntentEvidence {
            workflow,
            questions,
            assessment: None,
            requirements: vec![],
        })
    }
}

pub(super) fn selection(
    workflow: &WorkflowIntent,
    assessment: &contract::AssessmentResult,
    retrieval: &RetrievalReceipt,
) -> Result<CapabilitySelection> {
    let mut coverage = Vec::new();
    let aliases = super::selector_contract::candidate_aliases(retrieval);
    for entry in &assessment.requirement_coverage {
        let statement = if entry.requirement == "f0" {
            assessment
                .operational_goal
                .as_deref()
                .context("missing operational goal")?
        } else {
            &workflow
                .linked()?
                .requirements
                .iter()
                .find(|r| r.id == entry.requirement)
                .context("unknown requirement")?
                .statement
        };
        let resolve = |ids: &[String]| -> Result<Vec<String>> {
            ids.iter()
                .map(|id| aliases.get(id).cloned().context("unknown candidate alias"))
                .collect()
        };
        let assessment = match &entry.assessment {
            contract::Assessment::Supported { supported_by } => RequirementAssessment::Supported {
                supported_by: resolve(supported_by)?,
            },
            contract::Assessment::Unresolved {
                useful_capabilities,
                missing,
            } => RequirementAssessment::Unresolved {
                useful_capabilities: resolve(useful_capabilities)?,
                missing: missing.clone(),
            },
            contract::Assessment::NoCapabilityNeeded {
                no_capability_needed,
            } => RequirementAssessment::NoCapabilityNeeded {
                no_capability_needed: no_capability_needed.clone(),
            },
        };
        coverage.push(RequirementCoverage {
            requirement: format!("{}: {}", entry.requirement, statement),
            assessment,
        });
    }
    CapabilitySelection::from_coverage(coverage, retrieval)
}
