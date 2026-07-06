//! Dry-run simulation under a project flow policy arm (draft or published).

use serde::Serialize;

use crate::execute_session::ExecuteSession;
use crate::flow_policy_repository::FlowPolicyRow;
use crate::http_execute::{apply_capability_seeds, CapabilitySeed, RankedCapabilitiesArg};
use crate::plan_flow_policy::{FlowPolicySnapshot, PolicyRevision};
use crate::plan_ux_reflection::{plan_ux_reflection_value, PlanUxBuildContext};
use crate::plasm_compile::compile_plasm_expression;
use crate::plasm_plan_run::evaluate_plasm_comp_dry;
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

pub async fn simulate_flow_policy(
    st: &PlasmHostState,
    row: &FlowPolicyRow,
    arm: SimulatePolicyArm,
    seeds: Vec<CapabilitySeed>,
    program: &str,
    intent: &str,
) -> Result<FlowPolicySimulateResult, String> {
    let snapshot = policy_snapshot_for_arm(row, arm)?;
    let out = apply_capability_seeds(
        st,
        None,
        None,
        seeds,
        None,
        None,
        None,
        intent,
        RankedCapabilitiesArg::Unspecified,
    )
    .await
    .map_err(|e| e.to_string())?;

    let ph = out.prompt_hash.clone();
    let sid = out.session_id.clone();

    let es_arc = st
        .get_execute_session(&ph, &sid)
        .await
        .ok_or_else(|| "simulate session missing after open".to_string())?;

    let mut es: ExecuteSession = (*es_arc).clone();
    es.flow_policy = snapshot;
    let ph_typed = ph
        .parse::<crate::execute_path_ids::PromptHashHex>()
        .map_err(|e| e.to_string())?;
    let sid_typed = sid
        .parse::<crate::execute_path_ids::ExecuteSessionId>()
        .map_err(|e| e.to_string())?;
    st.sessions.replace_session(&ph_typed, &sid_typed, es).await;

    let es = st
        .get_execute_session(&ph, &sid)
        .await
        .ok_or_else(|| "simulate session missing after policy pin".to_string())?;

    let pipeline = st.engine.prompt_pipeline();
    let cross = st.sessions.symbol_map_cross_cache();
    let bundle = compile_plasm_expression(
        pipeline,
        Some(cross),
        es.as_ref(),
        "flow_policy_simulate",
        program,
    )?;
    let dry = evaluate_plasm_comp_dry(es.as_ref(), &bundle)?;
    let gate = dry.evaluate_gate();
    let ux_ctx = PlanUxBuildContext {
        session: Some(es.as_ref()),
        param_bindings: &[],
    };
    let plan_ux = plan_ux_reflection_value(&dry, &ux_ctx);
    let comp = crate::plasm_comp_wire::trace_comp_wire_from_dry(&dry);
    let dry_verdict = format!("{:?}", gate.verdict).to_ascii_lowercase();

    // Ephemeral sandbox — do not leave simulate sessions in the global store.
    st.sessions.remove_by_strs(&ph, &sid).await;

    Ok(FlowPolicySimulateResult {
        dry_verdict,
        plan_ux_reflection: plan_ux,
        comp: serde_json::to_value(&comp).map_err(|e| e.to_string())?,
    })
}

fn policy_snapshot_for_arm(
    row: &FlowPolicyRow,
    arm: SimulatePolicyArm,
) -> Result<FlowPolicySnapshot, String> {
    match arm {
        SimulatePolicyArm::Draft => {
            let Some(policy) = row.draft_policy.clone() else {
                return Err("no draft policy — author rules before simulating draft arm".into());
            };
            Ok(FlowPolicySnapshot::Active {
                revision: PolicyRevision(row.published_revision.saturating_add(1)),
                policy,
            })
        }
        SimulatePolicyArm::Published => Ok(row.published_snapshot()),
    }
}
