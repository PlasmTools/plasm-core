//! Resolve `plasm_context` seeds (intent-only vs explicit; route-before-commit).

use plasm_core::discovery::{CgsCatalog, CgsDiscovery};
use rust_mcp_sdk::schema::schema_utils::CallToolError;
#[cfg(feature = "semantic-auto-seed")]
use rust_mcp_sdk::schema::{CallToolResult, TextContent};

use crate::http_execute::CapabilitySeed;
#[cfg(feature = "semantic-auto-seed")]
use crate::pending_clarify::{
    resolve_clarify_choice, ClarifyBinding, ClarifyRedeemError, PendingClarifyChoice,
    PendingClarifyRegistry,
};

const SEEDS_REJECTED_ON_AUTO_SEED: &str = "do not pass `seeds` when semantic auto-seed is enabled — pass `intent` only; the host selects capabilities. On clarify/hard_miss, rephrase `intent` with the provider brand (and entity names from the breakout browse preview as prose), or pass `routing_ref` + `clarify_choice` from the breakout.";

const MISSING_SEEDS_ENABLE_AUTO_SEED: &str = "missing capability picks: pass non-empty `seeds` or enable semantic auto-seed (`PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1` with `semantic-auto-seed` build)";

const MISSING_SEEDS_NO_FEATURE: &str = "missing capability picks: `plasm_context` requires non-empty `seeds` unless the host is built with `semantic-auto-seed`";

/// Session phase for seed resolution (new vs delta extend).
#[derive(Debug, Clone, Copy)]
pub(crate) enum ContextPhase<'a> {
    New,
    Extend { exposed: &'a [(String, String)] },
}

impl ContextPhase<'_> {
    #[cfg(feature = "semantic-auto-seed")]
    #[must_use]
    fn clarify_binding(self, logical_session_id: Option<uuid::Uuid>) -> ClarifyBinding {
        match self {
            Self::New => ClarifyBinding::PreMintNew,
            Self::Extend { .. } => ClarifyBinding::BoundExtend {
                logical_session_id: logical_session_id.expect("extend requires logical session id"),
            },
        }
    }

    #[must_use]
    fn session_mode(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Extend { .. } => "extend",
        }
    }

    #[must_use]
    fn exclude_exposed(&self) -> Option<&[(String, String)]> {
        match self {
            Self::New => None,
            Self::Extend { exposed } => Some(*exposed),
        }
    }
}

/// Explicit seeds vs auto-route (feature builds only).
pub(crate) enum SeedsPolicy<'a> {
    Explicit(Option<Vec<CapabilitySeed>>),
    #[cfg(feature = "semantic-auto-seed")]
    Auto(AutoSeedRouteArgs<'a>),
}

/// Arguments for semantic auto-seed routing.
#[cfg(feature = "semantic-auto-seed")]
pub(crate) struct AutoSeedRouteArgs<'a> {
    pub tool: &'a str,
    pub intent: &'a str,
    pub logical_session_ref: Option<&'a str>,
    pub logical_session_id: Option<uuid::Uuid>,
    pub allowed_entry_ids: Option<Vec<String>>,
    pub pending_clarify: &'a PendingClarifyRegistry,
    pub routing_ref: Option<&'a str>,
    pub clarify_choice: Option<&'a str>,
}

/// Domain routing decision — never carries MCP wire types.
#[derive(Debug)]
pub(crate) enum ContextRouteDecision {
    Expand {
        workflow_seeds: Vec<CapabilitySeed>,
        teaching_satellites: Vec<CapabilitySeed>,
        /// Validated supporting capability ids from the selector; `None` after clarify redeem.
        ranked_capabilities: Option<Vec<String>>,
    },
    /// Extend: intent committed, no teaching delta.
    Noop,
    /// Abstain breakout — present separately; do not mint/append.
    #[cfg(feature = "semantic-auto-seed")]
    Abstain(AbstainPlan),
}

#[cfg(feature = "semantic-auto-seed")]
#[derive(Debug)]
pub(crate) struct AbstainPlan {
    pub outcome: crate::discovery_routing::AutoSeedRouteOutcome,
    pub breakout: crate::discovery_routing::BreakoutContext,
    pub discover_preview: Option<String>,
}

impl ContextRouteDecision {
    /// Teaching exposure order for Expand.
    pub(crate) fn into_expand(
        self,
    ) -> Result<(Vec<CapabilitySeed>, Option<Vec<String>>), Self> {
        match self {
            Self::Expand {
                workflow_seeds,
                teaching_satellites,
                ranked_capabilities,
            } => {
                let mut seeds = workflow_seeds;
                for sat in teaching_satellites {
                    if seeds
                        .iter()
                        .any(|s| s.entry_id == sat.entry_id && s.entity == sat.entity)
                    {
                        continue;
                    }
                    seeds.push(sat);
                }
                Ok((seeds, ranked_capabilities))
            }
            other => Err(other),
        }
    }
}

pub(crate) fn semantic_auto_seed_on() -> bool {
    super::tools::mcp_semantic_auto_seed_enabled()
}

/// Resolve seeds for `plasm_context` (route before mint/append).
pub(crate) async fn resolve_context_seeds<C>(
    tool: &str,
    catalog: &C,
    intent: &str,
    allowed_entry_ids: Option<Vec<String>>,
    phase: ContextPhase<'_>,
    policy: SeedsPolicy<'_>,
) -> Result<ContextRouteDecision, CallToolError>
where
    C: CgsDiscovery + CgsCatalog + Send + Sync,
{
    match policy {
        #[cfg(feature = "semantic-auto-seed")]
        SeedsPolicy::Auto(auto) => {
            if !semantic_auto_seed_on() {
                return Err(CallToolError::invalid_arguments(
                    tool,
                    Some(MISSING_SEEDS_ENABLE_AUTO_SEED.into()),
                ));
            }
            route_auto_seed(catalog, phase, auto).await
        }
        SeedsPolicy::Explicit(explicit_seeds) => {
            if semantic_auto_seed_on() {
                if explicit_seeds.is_some() {
                    return Err(CallToolError::invalid_arguments(
                        tool,
                        Some(SEEDS_REJECTED_ON_AUTO_SEED.into()),
                    ));
                }
                #[cfg(feature = "semantic-auto-seed")]
                {
                    return Err(CallToolError::invalid_arguments(
                        tool,
                        Some("internal: auto-seed on requires SeedsPolicy::Auto".into()),
                    ));
                }
                #[cfg(not(feature = "semantic-auto-seed"))]
                {
                    let _ = (catalog, intent, allowed_entry_ids, phase);
                    return Err(CallToolError::invalid_arguments(
                        tool,
                        Some(MISSING_SEEDS_NO_FEATURE.into()),
                    ));
                }
            }
            let _ = (catalog, intent, allowed_entry_ids, phase);
            match explicit_seeds {
                Some(seeds) => Ok(ContextRouteDecision::Expand {
                    workflow_seeds: seeds,
                    teaching_satellites: Vec::new(),
                    ranked_capabilities: None,
                }),
                None => Err(CallToolError::invalid_arguments(
                    tool,
                    Some(
                        if cfg!(feature = "semantic-auto-seed") {
                            MISSING_SEEDS_ENABLE_AUTO_SEED
                        } else {
                            MISSING_SEEDS_NO_FEATURE
                        }
                        .into(),
                    ),
                )),
            }
        }
    }
}

#[cfg(feature = "semantic-auto-seed")]
async fn route_auto_seed<C>(
    catalog: &C,
    phase: ContextPhase<'_>,
    args: AutoSeedRouteArgs<'_>,
) -> Result<ContextRouteDecision, CallToolError>
where
    C: CgsDiscovery + CgsCatalog + Send + Sync,
{
    let expected_binding = phase.clarify_binding(args.logical_session_id);

    if let (Some(routing_ref), Some(choice)) = (args.routing_ref, args.clarify_choice) {
        return resolve_from_clarify_receipt(
            args.tool,
            args.pending_clarify,
            routing_ref,
            choice,
            &expected_binding,
        );
    }
    if args.clarify_choice.is_some() && args.routing_ref.is_none() {
        return Err(CallToolError::invalid_arguments(
            args.tool,
            Some("`clarify_choice` requires `routing_ref` from a prior clarify breakout".into()),
        ));
    }
    if args.routing_ref.is_some() && args.clarify_choice.is_none() {
        return Err(CallToolError::invalid_arguments(
            args.tool,
            Some("`routing_ref` requires `clarify_choice` (1-based index or catalog:entity)".into()),
        ));
    }

    let allowed_slice = args.allowed_entry_ids.clone();
    let outcome = crate::discovery_seed_select::route_intent_to_seeds(
        catalog,
        args.intent,
        args.allowed_entry_ids,
        phase.exclude_exposed(),
    )
    .await;

    match outcome {
        crate::discovery_routing::AutoSeedRouteOutcome::Noop { .. } => {
            Ok(ContextRouteDecision::Noop)
        }
        crate::discovery_routing::AutoSeedRouteOutcome::Ready {
            seeds: pairs,
            teaching_satellites,
            supporting_capability_ids,
            ..
        } => {
            // Validated Ready always has non-empty supporting; never invent IDs.
            Ok(ContextRouteDecision::Expand {
                workflow_seeds: pairs
                    .into_iter()
                    .map(|(entry_id, entity)| CapabilitySeed { entry_id, entity })
                    .collect(),
                teaching_satellites: teaching_satellites
                    .into_iter()
                    .map(|(entry_id, entity)| CapabilitySeed { entry_id, entity })
                    .collect(),
                ranked_capabilities: Some(supporting_capability_ids),
            })
        }
        abstain => {
            let discover_preview = crate::discovery_routing::discover_preview_markdown(
                catalog,
                args.intent,
                allowed_slice.as_deref(),
            );
            let routing_ref = match &abstain {
                crate::discovery_routing::AutoSeedRouteOutcome::Clarify {
                    alternative_sets, ..
                } if !alternative_sets.is_empty() => Some(args.pending_clarify.insert(
                    PendingClarifyChoice::new(
                        alternative_sets.clone(),
                        args.intent,
                        expected_binding,
                    ),
                )),
                _ => None,
            };
            let breakout = crate::discovery_routing::BreakoutContext {
                session_mode: phase.session_mode(),
                logical_session_ref: args.logical_session_ref.map(|s| s.to_string()),
                routing_ref,
            };
            Ok(ContextRouteDecision::Abstain(AbstainPlan {
                outcome: abstain,
                breakout,
                discover_preview,
            }))
        }
    }
}

#[cfg(feature = "semantic-auto-seed")]
fn resolve_from_clarify_receipt(
    tool: &str,
    pending: &PendingClarifyRegistry,
    routing_ref: &str,
    choice: &str,
    expected: &ClarifyBinding,
) -> Result<ContextRouteDecision, CallToolError> {
    let receipt = pending.redeem(routing_ref, expected).map_err(|e| {
        CallToolError::invalid_arguments(tool, Some(e.to_message()))
    })?;
    let pairs = resolve_clarify_choice(&receipt.alternatives, choice).map_err(|e| {
        CallToolError::invalid_arguments(tool, Some(ClarifyRedeemError::Choice(e).to_message()))
    })?;
    // No forged supporting capability ids — ranked stays unspecified until agent/host re-ranks.
    Ok(ContextRouteDecision::Expand {
        workflow_seeds: pairs
            .into_iter()
            .map(|(entry_id, entity)| CapabilitySeed { entry_id, entity })
            .collect(),
        teaching_satellites: Vec::new(),
        ranked_capabilities: None,
    })
}

/// Present an abstain plan as an MCP tool result (presentation layer only).
#[cfg(feature = "semantic-auto-seed")]
pub(crate) fn present_abstain(plan: AbstainPlan, intent: &str) -> CallToolResult {
    let text = crate::discovery_routing::build_auto_seed_breakout_markdown_with_context(
        &plan.outcome,
        intent,
        plan.discover_preview.as_deref(),
        &plan.breakout,
    );
    let routing = crate::discovery_routing::build_routing_meta_with_context(
        &plan.outcome,
        "semantic",
        plan.discover_preview.as_deref(),
        plan.breakout.routing_ref.as_deref(),
    );
    let mut meta = serde_json::Map::new();
    meta.insert(
        "plasm".to_string(),
        serde_json::json!({ "routing": routing }),
    );
    let mut res = CallToolResult::text_content(vec![TextContent::new(text, None, None)]);
    res = res.with_meta(Some(meta));
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_exclude_exposed_only_on_extend() {
        let exposed = [("github".into(), "Repository".into())];
        assert!(ContextPhase::New.exclude_exposed().is_none());
        assert_eq!(
            ContextPhase::Extend {
                exposed: exposed.as_slice()
            }
            .exclude_exposed()
            .unwrap()
            .len(),
            1
        );
    }

    #[cfg(feature = "semantic-auto-seed")]
    #[test]
    fn clarify_binding_matches_phase() {
        let sid = uuid::Uuid::new_v4();
        assert_eq!(
            ContextPhase::New.clarify_binding(None),
            ClarifyBinding::PreMintNew
        );
        assert_eq!(
            ContextPhase::Extend { exposed: &[] }.clarify_binding(Some(sid)),
            ClarifyBinding::BoundExtend {
                logical_session_id: sid
            }
        );
    }
}
