//! Plan execute gate: merged dry verdict + sealed flow admission (single path).

use crate::plan_dry_display::{PlanDryReview, PlanDryVerdict};
use crate::plan_flow::{FlowAdmission, FlowCheckedPlan, FlowDenial, FlowVerdict, PlanFlowAnalysis};
use plasm_core::PlanCommitRef;

/// Flow + boundedness review merged into one gate verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedPlanGate {
    pub verdict: PlanDryVerdict,
    pub admission: Result<FlowAdmission, FlowDenial>,
}

#[derive(Debug, Clone, Copy)]
pub struct PlanGateContext<'a> {
    pub force: bool,
    pub plan_commit_ref: Option<&'a PlanCommitRef>,
}

impl PlanGateContext<'_> {
    pub fn without_commit(force: bool) -> PlanGateContext<'static> {
        PlanGateContext {
            force,
            plan_commit_ref: None,
        }
    }
}

pub fn plan_dry_verdict_from_flow(analysis: &PlanFlowAnalysis) -> PlanDryVerdict {
    match analysis.verdict {
        FlowVerdict::Clean => PlanDryVerdict::Ok,
        FlowVerdict::NeedsReview => PlanDryVerdict::Review,
        FlowVerdict::Denied => PlanDryVerdict::Deny,
    }
}

/// Merge flow analysis with structural dry review (same rule as compact dry view).
pub fn merged_gate_verdict(
    flow: &PlanFlowAnalysis,
    review: &PlanDryReview,
    return_unbounded: bool,
) -> PlanDryVerdict {
    let flow_verdict = plan_dry_verdict_from_flow(flow);
    let review_verdict = if review.needs_review(return_unbounded) {
        PlanDryVerdict::Review
    } else {
        PlanDryVerdict::Ok
    };
    std::cmp::max(flow_verdict, review_verdict)
}

/// Single construction site for admission + merged verdict.
pub fn evaluate_plan_gate(
    flow: &PlanFlowAnalysis,
    review: &PlanDryReview,
    return_unbounded: bool,
) -> EvaluatedPlanGate {
    let admission = FlowCheckedPlan {
        analysis: flow.clone(),
    }
    .admit();
    EvaluatedPlanGate {
        verdict: merged_gate_verdict(flow, review, return_unbounded),
        admission,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanGateDecision {
    Proceed(FlowAdmission),
    NeedsReview,
    Denied(FlowDenial),
}

#[must_use]
pub fn plan_gate(gate: &EvaluatedPlanGate, ctx: PlanGateContext<'_>) -> PlanGateDecision {
    match gate.verdict {
        PlanDryVerdict::Ok => match &gate.admission {
            Ok(admission) => PlanGateDecision::Proceed(admission.clone()),
            Err(denial) => PlanGateDecision::Denied(denial.clone()),
        },
        PlanDryVerdict::Review => {
            if ctx.force || ctx.plan_commit_ref.is_some() {
                match &gate.admission {
                    Ok(admission) => PlanGateDecision::Proceed(admission.clone()),
                    Err(denial) => PlanGateDecision::Denied(denial.clone()),
                }
            } else {
                PlanGateDecision::NeedsReview
            }
        }
        PlanDryVerdict::Deny => match gate.admission.clone() {
            Err(denial) => PlanGateDecision::Denied(denial),
            Ok(_) => PlanGateDecision::Denied(FlowDenial {
                verdict: FlowVerdict::Denied,
                violations: Vec::new(),
            }),
        },
    }
}

#[must_use]
pub fn plan_requires_review_gate(gate: &EvaluatedPlanGate, ctx: PlanGateContext<'_>) -> bool {
    matches!(plan_gate(gate, ctx), PlanGateDecision::NeedsReview)
}
