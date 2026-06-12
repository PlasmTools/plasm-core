//! HTTP contract for client-compiled Plasm effect [`Plan`](crate::plasm_plan::Plan) execution.
//!
//! CLI `plasm run` resolves local `e#` / `p#` symbols, validates the plan locally, then POSTs
//! typed plan JSON here. The server never parses symbolic surface text on this path.

use serde::{Deserialize, Serialize};

use crate::catalog_pin::{CatalogPin, CatalogPinError};
use crate::execute_session::ExecuteSession;
use crate::plasm_comp_bundle::PlasmCompBundle;
use crate::plasm_comp_wire::plasm_comp_artifact_from_comp;

/// Wire protocol version for [`ResolvedPlanRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPlanProtocolVersion(u16);

impl ResolvedPlanProtocolVersion {
    pub const V1: Self = Self(1);

    pub fn from_wire(v: u16) -> Result<Self, ResolvedPlanReject> {
        if v == Self::V1.0 {
            Ok(Self::V1)
        } else {
            Err(ResolvedPlanReject::UnsupportedProtocolVersion {
                got: v,
                expected: Self::V1.0,
            })
        }
    }

    pub const fn as_u16(self) -> u16 {
        self.0
    }
}

pub const RESOLVED_PLAN_CONTENT_TYPE: &str = "application/vnd.plasm.resolved-plan+json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedPlanRunMode {
    Plan,
    Run,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPlanRequest {
    pub protocol_version: u16,
    pub client_session_id: String,
    pub catalog_pins: Vec<CatalogPin>,
    pub mode: ResolvedPlanRunMode,
    pub source_program: String,
    /// Canonical typed comp (legacy wire alias: `plan`).
    #[serde(alias = "plan")]
    pub comp: plasm_core::PlasmComp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedPlanResponse {
    pub plan: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub dry_run: bool,
    pub comp: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_results: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_summary: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_markdown: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

/// Server-side acceptance of a resolved-plan POST (pins + typed plan artifact).
#[derive(Debug)]
pub(crate) struct PreparedResolvedPlan {
    pub bundle: PlasmCompBundle,
    pub mode: ResolvedPlanRunMode,
    #[allow(dead_code)]
    pub source_program: String,
}

#[derive(Debug)]
pub(crate) enum ResolvedPlanReject {
    UnsupportedProtocolVersion { got: u16, expected: u16 },
    CatalogPins(CatalogPinError),
    InvalidPlan(String),
}

impl fmt::Display for ResolvedPlanReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedProtocolVersion { got, expected } => write!(
                f,
                "unsupported protocol_version {got} (expected {expected})"
            ),
            Self::CatalogPins(e) => e.fmt(f),
            Self::InvalidPlan(msg) => f.write_str(msg),
        }
    }
}

use std::fmt;

/// Validate wire request against an execute session and lift to [`PreparedResolvedPlan`].
pub(crate) fn prepare_resolved_plan_request(
    req: ResolvedPlanRequest,
    sess: &ExecuteSession,
) -> Result<PreparedResolvedPlan, ResolvedPlanReject> {
    ResolvedPlanProtocolVersion::from_wire(req.protocol_version)?;
    sess.validate_catalog_pins(&req.catalog_pins)
        .map_err(ResolvedPlanReject::CatalogPins)?;
    let artifact =
        plasm_comp_artifact_from_comp(req.comp).map_err(ResolvedPlanReject::InvalidPlan)?;
    Ok(PreparedResolvedPlan {
        bundle: PlasmCompBundle::new(artifact).map_err(ResolvedPlanReject::InvalidPlan)?,
        mode: req.mode,
        source_program: req.source_program,
    })
}
