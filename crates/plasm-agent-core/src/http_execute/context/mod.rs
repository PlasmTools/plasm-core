//! Execute session context: capability seeds, exposure, plasm_context MCP surface.

#![allow(unused_imports)] // `pub(crate)` re-exports consumed by sibling modules and integration tests.

mod backend;
mod seed_resolve;
mod seeds;
mod session;

#[cfg(test)]
mod ranked_replay_fixtures;

pub(crate) use backend::cgs_entity_names_sample;
pub(crate) use backend::{
    patch_cgs_context_outbound_hosted, patch_cgs_context_resolved_http_backend,
    resolve_http_backend_for_entry,
};
pub use seed_resolve::{resolve_capability_seeds, resolve_entity_name_case_insensitive};
pub(crate) use seeds::{
    build_capability_exposure_plan, build_plasm_context_agent_markdown,
    build_plasm_context_tool_meta, format_session_churn_advisory,
    format_session_unchanged_reuse_markdown, group_seed_entities_by_entry,
    normalize_context_intent_for_domain_filter, normalize_ranked_capabilities_for_gate,
    primary_entry_id_for_grouped, teaching_exposure_at, unchanged_expand_wave,
    CapabilityExposurePlan, PlasmContextToolMetaParams, STALE_EXECUTE_BINDING_NOTICE,
};
pub use seeds::{normalize_capability_seeds, RankedCapabilitiesArg};
pub use session::apply_capability_seeds;
pub(crate) use session::execute_session_create_response_inner;
#[cfg(test)]
pub(crate) use session::replay_teaching_exposure_waves;
pub(crate) use session::upsert_logical_ledger_from_snapshot;
pub(crate) use session::{
    apply_federate_exposure_wave, build_initial_exposure_wave, ExposureCatalogWave,
};
pub use session::{
    execute_session_create_response, expand_execute_teaching_session, federate_execute_session,
    ExpandTeachingWaveResult,
};
