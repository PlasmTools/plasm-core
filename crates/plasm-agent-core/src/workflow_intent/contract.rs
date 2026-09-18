//! Production-shaped request/decoder functions used by offline experiments.
//! No provider transport, repair, prompt copies, or domain recipes live here.
use super::WorkflowIntent;
use crate::discovery_service::selector_contract;
use crate::discovery_store::RetrievalReceipt;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const INTERPRET: &str = include_str!("interpret.txt");
const AUDIT: &str = include_str!("audit.txt");
const SEMANTICS: &str = include_str!("semantics.txt");
const ASSESS: &str = include_str!("assess.txt");

/// Retain this packet with the provider response; decoding checks the exact
/// workflow/candidate snapshot that produced the request, including stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundRequest {
    pub body: String,
    context_digest: String,
    operational_goal: Option<String>,
}
fn context_digest(
    stage: &str,
    workflow: &WorkflowIntent,
    retrieval: Option<&RetrievalReceipt>,
    operational_goal: Option<&str>,
) -> Result<String> {
    Ok(plasm_core::catalog_discovery::content_hash(
        &serde_json::to_vec(&(stage, workflow, retrieval, operational_goal))?,
    ))
}
fn bind(
    body: String,
    stage: &str,
    workflow: &WorkflowIntent,
    retrieval: Option<&RetrievalReceipt>,
) -> Result<BoundRequest> {
    Ok(BoundRequest {
        body,
        context_digest: context_digest(stage, workflow, retrieval, None)?,
        operational_goal: None,
    })
}
fn check_binding(
    issued: &BoundRequest,
    stage: &str,
    workflow: &WorkflowIntent,
    retrieval: Option<&RetrievalReceipt>,
) -> Result<()> {
    ensure!(
        issued.context_digest
            == context_digest(
                stage,
                workflow,
                retrieval,
                issued.operational_goal.as_deref()
            )?,
        "response belongs to a different workflow revision, interpretation, or candidate snapshot"
    );
    Ok(())
}

pub(super) fn object(properties: Value) -> Value {
    let required: Vec<_> = properties
        .as_object()
        .expect("schema properties")
        .keys()
        .cloned()
        .collect();
    json!({"type":"object","additionalProperties":false,"required":required,"properties":properties})
}
pub(super) fn ids(values: Vec<String>) -> Value {
    if values.is_empty() {
        json!({"type":"array","maxItems":0,"items":{"type":"string"}})
    } else {
        json!({"type":"array","items":{"type":"string","enum":values}})
    }
}
pub(super) fn request(
    model: &str,
    instructions: &str,
    input: Value,
    schema: Value,
    name: &str,
) -> Result<String> {
    ensure!(!model.trim().is_empty(), "model required");
    Ok(json!({"model":model,"temperature":0,"seed":42,"provider":{"require_parameters":true},
        "messages":[{"role":"system","content":instructions},{"role":"user","content":serde_json::to_string(&input)?}],
        "response_format":{"type":"json_schema","json_schema":{"name":name,"strict":true,"schema":schema}}}).to_string())
}

pub(super) fn commit_base_request(model: &str, workflow: &WorkflowIntent) -> Result<BoundRequest> {
    let protocol = include_str!("commit.txt");
    workflow.validate()?;
    let prior_ids: Vec<String> = workflow
        .interpretation()
        .map(|i| super::conditional::units(i).into_keys().collect())
        .unwrap_or_default();
    let target_revision = workflow.next_interpretation_revision();
    let visible_turns = &workflow.turns()[..target_revision as usize];
    let mut sources = ids(visible_turns.iter().map(|t| t.id.clone()).collect());
    sources["minItems"] = json!(1);
    let uncertainty = object(json!({
        "question":{"type":"string","minLength":1},
        "alternatives":{"type":"array","minItems":2,"maxItems":8,"items":{"type":"string","minLength":1}}
    }));
    let item = object(
        json!({"uncertainty":{"anyOf":[{"type":"null"},uncertainty]},"kind":{"type":"string","enum":["effect","prohibition","selection_constraint","condition","information_need"]},
        "statement":{"type":"string","minLength":1},"source_turn_ids":sources.clone()}),
    );
    let decision = json!({"anyOf":[
        object(json!({"action":{"type":"string","enum":["retain"]}})),
        object(json!({"action":{"type":"string","enum":["retire"]},"reason":{"type":"string","minLength":1},"source_turn_ids":sources}))
    ]});
    let decisions: serde_json::Map<String, Value> = prior_ids
        .into_iter()
        .map(|id| (id, decision.clone()))
        .collect();
    let schema = object(json!({"dispositions":object(Value::Object(decisions)),
        "requirements":{"type":"array","maxItems":64,"items":item},
        "no_requirements_reason":{"type":["string","null"]}}));
    let body = request(
        model,
        &format!("{INTERPRET}\n{SEMANTICS}\n{protocol}"),
        json!({"workflow":{"turns":visible_turns,"interpretation":workflow.interpretation(),"target_revision":target_revision},"discovery_focus":null}),
        schema,
        "intent_interpretation",
    )?;
    bind(body, "interpret", workflow, None)
}

#[derive(Deserialize)]
struct Envelope {
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    finish_reason: String,
    message: Message,
}
#[derive(Deserialize)]
struct Message {
    content: String,
}
pub(super) fn content(raw: &str) -> Result<String> {
    let envelope: Envelope = serde_json::from_str(raw).context("invalid provider envelope")?;
    ensure!(envelope.choices.len() == 1, "expected one choice");
    let choice = envelope
        .choices
        .into_iter()
        .next()
        .context("missing choice")?;
    ensure!(
        choice.finish_reason == "stop",
        "provider response incomplete"
    );
    Ok(choice.message.content)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditFinding {
    pub disposition: AuditDisposition,
    pub kind: AuditFindingKind,
    pub requirement_ids: Vec<String>,
    pub explanation: String,
}
/// Model judgment only; an actionable finding is not authorization to repair.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDisposition {
    Actionable,
    Caution,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditFindingKind {
    Omitted,
    Invented,
    RevisionError,
    AmbiguityLost,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentAudit {
    pub findings: Vec<AuditFinding>,
}

pub fn audit_request(model: &str, workflow: &WorkflowIntent) -> Result<BoundRequest> {
    let current = workflow.current()?;
    let finding = object(
        json!({"disposition":{"type":"string","enum":["actionable","caution"]},"kind":{"type":"string","enum":["omitted","invented","revision_error","ambiguity_lost"]},
        "requirement_ids":ids(current.requirements.iter().map(|r|r.id.clone()).collect()),
        "explanation":{"type":"string","minLength":1}}),
    );
    let body = request(
        model,
        &format!("{AUDIT}\n{SEMANTICS}"),
        json!({"workflow":workflow}),
        object(json!({"findings":{"type":"array","items":finding}})),
        "intent_audit",
    )?;
    bind(body, "audit", workflow, None)
}

pub fn decode_audit(
    workflow: &WorkflowIntent,
    issued: &BoundRequest,
    raw: &str,
) -> Result<IntentAudit> {
    check_binding(issued, "audit", workflow, None)?;
    let current = workflow.current()?;
    let audit: IntentAudit = serde_json::from_str(&content(raw)?)?;
    for f in &audit.findings {
        ensure!(!f.explanation.trim().is_empty(), "blank audit finding");
        let ids: BTreeSet<_> = f.requirement_ids.iter().collect();
        ensure!(
            ids.len() == f.requirement_ids.len()
                && ids
                    .iter()
                    .all(|id| current.requirements.iter().any(|r| &r.id == *id)),
            "invalid audit requirement IDs"
        );
    }
    Ok(audit)
}

/// Queries contain semantic needs, never the transport wrapper or all chat history.
/// Including constraints individually avoids silently discarding minority needs.
pub fn retrieval_queries(workflow: &WorkflowIntent) -> Result<Vec<(String, String)>> {
    let current = workflow.linked()?;
    let mut queries =
        super::links::queries(current, current.links.as_deref().expect("linked checked"))?;
    let impacts = super::decisions::impacts(current);
    for (id, query) in &mut queries {
        let affected: Vec<_> = impacts
            .iter()
            .filter(|d| d.requirement_ids.contains(id))
            .collect();
        if !affected.is_empty() {
            *query = format!(
                "PROVISIONAL interpretation; investigate alternatives, not settled scope.\n{query}"
            );
            for impact in affected {
                query.push_str(&format!(
                    "\nUnresolved {}: {}\nAlternatives: {}",
                    impact.decision.id,
                    impact.decision.question.question,
                    impact.decision.question.alternatives.join(" | ")
                ));
            }
        }
    }
    Ok(queries)
}

pub fn assessment_request(
    model: &str,
    workflow: &WorkflowIntent,
    retrieval: &RetrievalReceipt,
    focus: &str,
) -> Result<BoundRequest> {
    let operational_goal = (!focus.trim().is_empty()).then(|| focus.to_owned());
    let current = workflow.linked()?;
    // Reuse the current production candidate aliasing and supported/gap schema.
    let candidates = selector_contract::wire_candidates(retrieval);
    let mut schema = selector_contract::schema(retrieval);
    let array = &mut schema["properties"]["requirement_coverage"];
    let mut requirement_ids: Vec<_> = current.requirements.iter().map(|r| r.id.as_str()).collect();
    if operational_goal.is_some() {
        requirement_ids.push("f0");
    }
    array["minItems"] = json!(requirement_ids.len());
    array["maxItems"] = json!(requirement_ids.len());
    if !requirement_ids.is_empty() {
        array["items"]["properties"]["requirement"] =
            json!({"type":"string","enum":requirement_ids});
    }
    // A single record with an explicit decision avoids ambiguous object-union
    // decoding by structured-output providers. Rust enforces status invariants.
    let ids = selector_contract::offered_ids(retrieval);
    let evidence = if ids.is_empty() {
        json!({"type":"array","maxItems":0,"items":{"type":"string"}})
    } else {
        json!({"type":"array","items":{"type":"string","enum":ids}})
    };
    array["items"]["properties"]["assessment"] = object(json!({
        "status":{"type":"string","enum":["supported","unresolved","local"]},
        "capability_ids":evidence,
        "explanation":{"type":"string","minLength":1,
            "description":"For supported: how the capabilities compose. For unresolved: the specific absent producer or operation. For local: why no external capability is needed."}
    }));
    let body = request(
        model,
        ASSESS,
        json!({"workflow":workflow,"operational_goal":operational_goal.as_ref().map(|statement| json!({"id":"f0","statement":statement})),"candidates":candidates,"unresolved_decisions":super::decisions::impacts(current)}),
        schema,
        "intent_capability_coverage",
    )?;
    let mut issued = bind(body, "assess", workflow, Some(retrieval))?;
    issued.context_digest = context_digest(
        "assess",
        workflow,
        Some(retrieval),
        operational_goal.as_deref(),
    )?;
    issued.operational_goal = operational_goal;
    Ok(issued)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum Assessment {
    Supported {
        supported_by: Vec<String>,
    },
    Unresolved {
        useful_capabilities: Vec<String>,
        missing: String,
    },
    NoCapabilityNeeded {
        no_capability_needed: String,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    pub requirement: String,
    pub assessment: Assessment,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentResult {
    /// Operational need, kept separate from user requirements and authorization.
    pub operational_goal: Option<String>,
    pub requirement_coverage: Vec<Coverage>,
    /// Computed by the host, never supplied by the assessor.
    pub unresolved_decisions: Vec<super::decisions::DecisionImpact>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssessmentDraft {
    requirement_coverage: Vec<CoverageDraft>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageDraft {
    requirement: String,
    assessment: AssessmentChoice,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssessmentChoice {
    status: AssessmentStatus,
    capability_ids: Vec<String>,
    explanation: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AssessmentStatus {
    Supported,
    Unresolved,
    Local,
}
impl CoverageDraft {
    fn resolve(self) -> Result<Coverage> {
        let choice = self.assessment;
        ensure!(
            !choice.explanation.trim().is_empty(),
            "blank assessment explanation"
        );
        let assessment = match choice.status {
            AssessmentStatus::Supported => Assessment::Supported {
                supported_by: choice.capability_ids,
            },
            AssessmentStatus::Unresolved => Assessment::Unresolved {
                useful_capabilities: choice.capability_ids,
                missing: choice.explanation,
            },
            AssessmentStatus::Local => {
                ensure!(
                    choice.capability_ids.is_empty(),
                    "local assessment cannot cite external capabilities"
                );
                Assessment::NoCapabilityNeeded {
                    no_capability_needed: choice.explanation,
                }
            }
        };
        Ok(Coverage {
            requirement: self.requirement,
            assessment,
        })
    }
}

/// Structural totality is deterministic; support judgments still require evaluation.
/// Candidate IDs are request-local aliases and remain bound to the frozen receipt.
pub fn decode_assessment(
    workflow: &WorkflowIntent,
    retrieval: &RetrievalReceipt,
    issued: &BoundRequest,
    raw: &str,
) -> Result<AssessmentResult> {
    check_binding(issued, "assess", workflow, Some(retrieval))?;
    let draft: AssessmentDraft = serde_json::from_str(&content(raw)?)?;
    let result = AssessmentResult {
        operational_goal: issued.operational_goal.clone(),
        requirement_coverage: draft
            .requirement_coverage
            .into_iter()
            .map(CoverageDraft::resolve)
            .collect::<Result<Vec<_>>>()?,
        unresolved_decisions: vec![],
    };
    let mut expected: BTreeSet<_> = workflow
        .linked()?
        .requirements
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    if issued.operational_goal.is_some() {
        expected.insert("f0");
    }
    let actual: BTreeSet<_> = result
        .requirement_coverage
        .iter()
        .map(|r| r.requirement.as_str())
        .collect();
    ensure!(
        actual.len() == result.requirement_coverage.len() && actual == expected,
        "assessment must cover each active requirement exactly once"
    );
    let offered: BTreeSet<_> = (0..retrieval.candidates.len())
        .map(|i| format!("c{i}"))
        .collect();
    for c in &result.requirement_coverage {
        let evidence = match &c.assessment {
            Assessment::Supported { supported_by } => {
                ensure!(!supported_by.is_empty(), "empty support");
                supported_by
            }
            Assessment::Unresolved {
                useful_capabilities,
                missing,
            } => {
                ensure!(!missing.trim().is_empty(), "blank gap");
                useful_capabilities
            }
            Assessment::NoCapabilityNeeded {
                no_capability_needed,
            } => {
                ensure!(
                    !no_capability_needed.trim().is_empty(),
                    "blank local assessment"
                );
                continue;
            }
        };
        let unique: BTreeSet<_> = evidence.iter().collect();
        ensure!(
            unique.len() == evidence.len() && evidence.iter().all(|id| offered.contains(id)),
            "unknown or duplicate capability ID"
        );
    }
    Ok(AssessmentResult {
        operational_goal: result.operational_goal,
        requirement_coverage: result.requirement_coverage,
        unresolved_decisions: super::decisions::impacts(workflow.linked()?),
    })
}

/// Link an already interpreted, host-identified requirement set. Never invent IDs.
pub fn linkage_request(model: &str, workflow: &WorkflowIntent) -> Result<BoundRequest> {
    let current = workflow.current()?;
    ensure!(current.links.is_none(), "interpretation already linked");
    let choices = super::links::choices(current);
    super::links::ensure_feasible(current, &choices)?;
    let offered: Vec<_> = choices.iter().map(|c| c.id.clone()).collect();
    let mut array = ids(offered);
    array["maxItems"] = json!(choices.len().min(256));
    let body = request(
        model,
        &format!("{}\n{SEMANTICS}", include_str!("link.txt")),
        json!({"workflow":workflow,"link_choices":choices,"fixed_branches":super::conditional::edges(&current.requirements,&current.conditionals)}),
        object(json!({"link_ids":array})),
        "intent_links",
    )?;
    bind(body, "link", workflow, None)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkDraft {
    link_ids: Vec<String>,
}
pub fn decode_linkage(
    workflow: &WorkflowIntent,
    issued: &BoundRequest,
    raw: &str,
) -> Result<WorkflowIntent> {
    check_binding(issued, "link", workflow, None)?;
    let draft: LinkDraft = serde_json::from_str(&content(raw)?)?;
    let choices = super::links::choices(workflow.current()?);
    let mut seen = BTreeSet::new();
    let mut links = super::conditional::edges(
        &workflow.current()?.requirements,
        &workflow.current()?.conditionals,
    );
    for id in draft.link_ids {
        ensure!(seen.insert(id.clone()), "duplicate link choice");
        let choice = choices
            .iter()
            .find(|c| c.id == id)
            .context("unknown link choice")?;
        links.push(choice.link.clone());
    }
    let mut updated = workflow.clone();
    updated.set_links(workflow.revision(), workflow.current()?.version, links)?;
    Ok(updated)
}
