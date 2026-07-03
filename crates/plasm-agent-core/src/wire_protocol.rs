//! Wire protocol version pins — exact-match cutover (no legacy acceptance).
//!
//! Each domain exposes a canonical `*_SCHEMA_VERSION` constant and a `validate_*` helper
//! that rejects stale or partial payloads at ingress.

pub use plasm_core::{PERSISTED_SYMBOL_LEDGER_VERSION, PLASM_COMP_WIRE_VERSION};

pub use crate::plan_flow_reflection::{
    validate_plan_ux_flow_reflection_wire, PLAN_UX_FLOW_REFLECTION_SCHEMA_VERSION,
};
pub use crate::plan_ux_reflection::{
    validate_plan_ux_reflection_wire, PLAN_UX_REFLECTION_SCHEMA_VERSION,
};
pub use crate::run_artifacts::{
    parse_run_artifact_document_bytes, validate_artifact_payload_metadata,
    validate_run_artifact_document, validate_run_artifact_document_json,
    RUN_ARTIFACT_PAYLOAD_SCHEMA_VERSION,
};
pub use crate::session_graph_persistence::{
    validate_graph_page_delta, GRAPH_PAGE_DELTA_SCHEMA_VERSION,
};
pub use crate::workflow_manifest::WORKFLOW_MANIFEST_SCHEMA_VERSION;
pub use crate::workflow_view_model::{
    validate_workflow_view_model, WORKFLOW_VIEW_MODEL_SCHEMA_VERSION,
};
