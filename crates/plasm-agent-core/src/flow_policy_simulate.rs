//! Dry-run simulation under a project flow policy arm (draft or published).

use serde::Serialize;

use crate::execute_session::ExecuteSession;
use crate::flow_policy_repository::FlowPolicyRow;
use crate::http_execute::{apply_capability_seeds, CapabilitySeed};
use crate::plan_flow_policy::{FlowPolicy, FlowPolicySnapshot, PolicyRevision};
use crate::plan_ux_reflection::{plan_ux_reflection_value, PlanUxBuildContext};
use crate::plasm_compile::compile_program;
use crate::plasm_plan_run::evaluate_plasm_comp_dry_snapshot;
use crate::server_state::PlasmHostState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulatePolicyArm {
    Draft,
    Published,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlowPolicySimulateResult {
    pub dry_verdict: String,
    pub plan_ux_reflection: serde_json::Value,
    pub comp: serde_json::Value,
}

/// Typed simulate failures returned to the HTTP layer as JSON `error` + `code`.
#[derive(Debug, thiserror::Error)]
pub enum SimulateError {
    #[error("no draft policy — author rules before simulating draft arm")]
    DraftMissing,
    #[error("no published policy — publish a revision before simulating published arm")]
    PublishedInactive,
    #[error("execute-session setup failed: {0}")]
    SessionOpen(#[source] Box<crate::http_execute::SessionMutateError>),
    #[error("simulate session is missing after {phase}")]
    SessionMissing { phase: SimulateSessionPhase },
    #[error("invalid prompt hash returned by session setup: {0}")]
    InvalidPromptHash(#[source] crate::execute_path_ids::PromptHashParseError),
    #[error("invalid session identifier returned by session setup: {0}")]
    InvalidSessionId(#[source] crate::execute_path_ids::ExecuteSessionIdParseError),
    #[error("flow-policy program compilation failed")]
    CompileFailed(crate::compilation_error::CompilationError),
    #[error("flow-policy dry evaluation failed")]
    DryEvaluationFailed(crate::program_diagnostic::ProgramStageError),
    #[error("simulate response encoding failed: {0}")]
    ResponseSerialization(#[source] serde_json::Error),
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum SimulateSessionPhase {
    #[error("session open")]
    Open,
    #[error("policy pin")]
    PolicyPin,
}

impl SimulateError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::DraftMissing => "draft_missing",
            Self::PublishedInactive => "published_inactive",
            Self::SessionOpen(_)
            | Self::SessionMissing { .. }
            | Self::InvalidPromptHash(_)
            | Self::InvalidSessionId(_) => "session_error",
            Self::CompileFailed(_) => "compile_failed",
            Self::DryEvaluationFailed(_) | Self::ResponseSerialization(_) => "simulate_failed",
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::DraftMissing => self.to_string(),
            Self::PublishedInactive => self.to_string(),
            Self::SessionOpen(error) => error.to_string(),
            Self::SessionMissing { phase } => match phase {
                SimulateSessionPhase::Open => "simulate session missing after open".into(),
                SimulateSessionPhase::PolicyPin => {
                    "simulate session missing after policy pin".into()
                }
            },
            Self::InvalidPromptHash(error) => error.to_string(),
            Self::InvalidSessionId(error) => error.to_string(),
            Self::CompileFailed(error) => error.to_string(),
            Self::DryEvaluationFailed(error) => error.to_string(),
            Self::ResponseSerialization(error) => {
                format!("simulate response encode failed: {error}")
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct SimulateOptions {
    /// When set with `Draft` arm, use this policy as an ephemeral snapshot (no DB write).
    pub ephemeral_policy: Option<FlowPolicy>,
}

pub async fn simulate_flow_policy(
    st: &PlasmHostState,
    row: &FlowPolicyRow,
    arm: SimulatePolicyArm,
    seeds: Vec<CapabilitySeed>,
    program: &str,
    intent: &str,
) -> Result<FlowPolicySimulateResult, SimulateError> {
    simulate_flow_policy_with_options(
        st,
        row,
        arm,
        seeds,
        program,
        intent,
        SimulateOptions::default(),
    )
    .await
}

pub async fn simulate_flow_policy_with_options(
    st: &PlasmHostState,
    row: &FlowPolicyRow,
    arm: SimulatePolicyArm,
    seeds: Vec<CapabilitySeed>,
    program: &str,
    intent: &str,
    opts: SimulateOptions,
) -> Result<FlowPolicySimulateResult, SimulateError> {
    let snapshot = policy_snapshot_for_arm(row, arm, opts.ephemeral_policy.as_ref())?;
    let out = apply_capability_seeds(st, None, None, seeds, None, None, None, intent)
        .await
        .map_err(|error| SimulateError::SessionOpen(Box::new(error)))?;

    let ph = out.prompt_hash.clone();
    let sid = out.session_id.clone();

    let es_arc = st
        .get_execute_session(&ph, &sid)
        .await
        .ok_or(SimulateError::SessionMissing {
            phase: SimulateSessionPhase::Open,
        })?;

    let mut es: ExecuteSession = (*es_arc).clone();
    es.flow_policy = snapshot;
    let ph_typed = ph
        .parse::<crate::execute_path_ids::PromptHashHex>()
        .map_err(SimulateError::InvalidPromptHash)?;
    let sid_typed = sid
        .parse::<crate::execute_path_ids::ExecuteSessionId>()
        .map_err(SimulateError::InvalidSessionId)?;
    st.sessions.replace_session(&ph_typed, &sid_typed, es).await;

    let es =
        st.get_execute_session(&ph, &sid)
            .await
            .ok_or_else(|| SimulateError::SessionMissing {
                phase: SimulateSessionPhase::PolicyPin,
            })?;

    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let bundle = compile_program(
        pipeline,
        Some(cross),
        es.as_ref(),
        "flow_policy_simulate",
        program,
    )
    .await
    .map_err(SimulateError::CompileFailed)?;
    let dry = evaluate_plasm_comp_dry_snapshot(es.as_ref(), &bundle)
        .await
        .map_err(SimulateError::DryEvaluationFailed)?;
    let gate = dry.evaluate_gate();
    let ux_ctx = PlanUxBuildContext {
        session: Some(es.as_ref()),
        param_bindings: &[],
    };
    let plan_ux = plan_ux_reflection_value(&dry, &ux_ctx);
    let comp = crate::plasm_comp_wire::trace_comp_wire_from_dry(&dry);
    let dry_verdict = gate.verdict.as_wire().to_string();

    // Ephemeral sandbox — do not leave simulate sessions in the global store.
    st.sessions.remove_by_strs(&ph, &sid).await;

    Ok(FlowPolicySimulateResult {
        dry_verdict,
        plan_ux_reflection: plan_ux,
        comp: serde_json::to_value(&comp).map_err(SimulateError::ResponseSerialization)?,
    })
}

pub fn policy_snapshot_for_arm(
    row: &FlowPolicyRow,
    arm: SimulatePolicyArm,
    ephemeral: Option<&FlowPolicy>,
) -> Result<FlowPolicySnapshot, SimulateError> {
    match arm {
        SimulatePolicyArm::Draft => {
            if let Some(policy) = ephemeral {
                return Ok(FlowPolicySnapshot::Active {
                    revision: PolicyRevision(row.published_revision.saturating_add(1)),
                    policy: policy.clone(),
                });
            }
            let Some(policy) = row.draft_policy.clone() else {
                return Err(SimulateError::DraftMissing);
            };
            Ok(FlowPolicySnapshot::Active {
                revision: PolicyRevision(row.published_revision.saturating_add(1)),
                policy,
            })
        }
        SimulatePolicyArm::Published => match row.published_snapshot() {
            snap @ FlowPolicySnapshot::Active { .. } => Ok(snap),
            FlowPolicySnapshot::Inactive => Err(SimulateError::PublishedInactive),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan_dry_display::PlanDryVerdict;
    use crate::plan_flow_policy::FlowPolicy;

    #[test]
    fn boxed_session_error_preserves_source_and_code() {
        use std::error::Error;
        let bytes = std::mem::size_of::<SimulateError>();
        assert!(bytes < 128, "SimulateError is {bytes} bytes; expected <128");
        let error = SimulateError::SessionOpen(Box::new(
            crate::http_execute::SessionMutateError::UnknownOrExpiredSession,
        ));
        assert!(matches!(
            error
                .source()
                .unwrap()
                .downcast_ref::<Box<crate::http_execute::SessionMutateError>>()
                .unwrap()
                .as_ref(),
            crate::http_execute::SessionMutateError::UnknownOrExpiredSession
        ));
        assert_eq!(error.code(), "session_error");
        assert_eq!(
            error.message(),
            crate::http_execute::SessionMutateError::UnknownOrExpiredSession.to_string()
        );
    }

    fn empty_row() -> FlowPolicyRow {
        FlowPolicyRow {
            tenant_id: "t".into(),
            workspace_slug: "w".into(),
            project_slug: "p".into(),
            published_revision: 0,
            published_policy: None,
            published_at: None,
            published_by_subject: None,
            draft_policy: None,
            draft_updated_at: None,
            draft_validated_at: None,
            draft_validation_ok: None,
        }
    }

    #[test]
    fn draft_missing_without_ephemeral() {
        let err =
            policy_snapshot_for_arm(&empty_row(), SimulatePolicyArm::Draft, None).unwrap_err();
        assert!(matches!(err, SimulateError::DraftMissing));
        assert_eq!(err.code(), "draft_missing");
    }

    #[test]
    fn draft_uses_ephemeral_policy() {
        let policy = FlowPolicy::empty_allow();
        let snap =
            policy_snapshot_for_arm(&empty_row(), SimulatePolicyArm::Draft, Some(&policy)).unwrap();
        match snap {
            FlowPolicySnapshot::Active { .. } => {}
            FlowPolicySnapshot::Inactive => panic!("expected active ephemeral snapshot"),
        }
    }

    #[test]
    fn published_inactive_fail_closed() {
        let err =
            policy_snapshot_for_arm(&empty_row(), SimulatePolicyArm::Published, None).unwrap_err();
        assert!(matches!(err, SimulateError::PublishedInactive));
        assert_eq!(err.code(), "published_inactive");
    }

    #[test]
    fn typed_path_errors_keep_the_existing_http_code_and_message() {
        let error = SimulateError::InvalidPromptHash(
            crate::execute_path_ids::PromptHashParseError::InvalidLength,
        );
        assert_eq!(error.code(), "session_error");
        assert_eq!(
            error.message(),
            "prompt_hash must be exactly 64 hexadecimal characters (SHA-256 digest)"
        );
    }

    #[test]
    fn dry_verdict_wire_matches_canonical() {
        assert_eq!(PlanDryVerdict::Ok.as_wire(), "ok");
        assert_eq!(PlanDryVerdict::Review.as_wire(), "review");
        assert_eq!(PlanDryVerdict::Deny.as_wire(), "deny");
    }
}
