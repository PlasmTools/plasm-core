//! Resolve `plasm_context` `session_mode: "new"` seeds (intent-only vs explicit).

use plasm_core::discovery::{CgsCatalog, CgsDiscovery};
use rust_mcp_sdk::schema::schema_utils::CallToolError;
use rust_mcp_sdk::schema::{CallToolResult, TextContent};

use crate::http_execute::CapabilitySeed;

const SEEDS_REJECTED_ON_AUTO_SEED_NEW: &str = "do not pass `seeds` on session_mode \"new\" when semantic auto-seed is enabled — pass `intent` only; the host selects seeds. On clarify/hard_miss, rephrase `intent` with the provider brand (and entity names from the breakout browse preview as prose). Use `seeds` only on session_mode \"extend\".";

const MISSING_SEEDS_ENABLE_AUTO_SEED: &str = "missing capability picks: pass non-empty `seeds` or enable semantic auto-seed (`PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1` with `semantic-auto-seed` build)";

const MISSING_SEEDS_NO_FEATURE: &str = "missing capability picks: `plasm_context` with `session_mode: \"new\"` requires non-empty `seeds` unless the host is built with `semantic-auto-seed`";

/// Outcome of resolving seeds for `session_mode: "new"`.
pub(crate) enum ContextNewSeeds {
    Ready {
        seeds: Vec<CapabilitySeed>,
        ranked_capabilities: Option<Vec<String>>,
    },
    /// Abstain breakout — caller returns this tool result as-is (no session mint).
    Abstain(CallToolResult),
}

pub(crate) fn semantic_auto_seed_on() -> bool {
    super::tools::mcp_semantic_auto_seed_enabled()
}

/// Resolve seeds for a new plasm_context open.
///
/// When auto-seed is on, non-empty `explicit_seeds` is rejected. When off, explicit seeds are
/// required.
pub(crate) async fn resolve_context_new_seeds<C>(
    tool: &str,
    catalog: &C,
    intent: &str,
    allowed_entry_ids: Option<Vec<String>>,
    explicit_seeds: Option<Vec<CapabilitySeed>>,
) -> Result<ContextNewSeeds, CallToolError>
where
    C: CgsDiscovery + CgsCatalog + Send + Sync,
{
    if semantic_auto_seed_on() {
        if explicit_seeds.is_some() {
            return Err(CallToolError::invalid_arguments(
                tool,
                Some(SEEDS_REJECTED_ON_AUTO_SEED_NEW.into()),
            ));
        }
        return route_auto_seed(tool, catalog, intent, allowed_entry_ids).await;
    }

    match explicit_seeds {
        Some(seeds) => Ok(ContextNewSeeds::Ready {
            seeds,
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

#[cfg(feature = "semantic-auto-seed")]
async fn route_auto_seed<C>(
    _tool: &str,
    catalog: &C,
    intent: &str,
    allowed_entry_ids: Option<Vec<String>>,
) -> Result<ContextNewSeeds, CallToolError>
where
    C: CgsDiscovery + CgsCatalog + Send + Sync,
{
    let allowed_slice = allowed_entry_ids.clone();
    let outcome = crate::discovery_seed_select::route_intent_to_seeds(
        catalog,
        intent,
        allowed_entry_ids,
    )
    .await;
    match outcome {
        crate::discovery_routing::AutoSeedRouteOutcome::Ready {
            seeds: pairs,
            supporting_capability_ids,
            ..
        } => Ok(ContextNewSeeds::Ready {
            seeds: pairs
                .into_iter()
                .map(|(entry_id, entity)| CapabilitySeed { entry_id, entity })
                .collect(),
            ranked_capabilities: Some(supporting_capability_ids),
        }),
        abstain => {
            let discover_preview = crate::discovery_routing::discover_preview_markdown(
                catalog,
                intent,
                allowed_slice.as_deref(),
            );
            let text = crate::discovery_routing::build_auto_seed_breakout_markdown(
                &abstain,
                intent,
                discover_preview.as_deref(),
            );
            let routing = crate::discovery_routing::build_routing_meta(
                &abstain,
                "semantic",
                discover_preview.as_deref(),
            );
            let mut meta = serde_json::Map::new();
            meta.insert(
                "plasm".to_string(),
                serde_json::json!({ "routing": routing }),
            );
            let mut res = CallToolResult::text_content(vec![TextContent::new(text, None, None)]);
            res = res.with_meta(Some(meta));
            Ok(ContextNewSeeds::Abstain(res))
        }
    }
}

#[cfg(not(feature = "semantic-auto-seed"))]
async fn route_auto_seed<C>(
    tool: &str,
    _catalog: &C,
    _intent: &str,
    _allowed_entry_ids: Option<Vec<String>>,
) -> Result<ContextNewSeeds, CallToolError>
where
    C: CgsDiscovery + CgsCatalog + Send + Sync,
{
    Err(CallToolError::invalid_arguments(
        tool,
        Some(MISSING_SEEDS_NO_FEATURE.into()),
    ))
}
