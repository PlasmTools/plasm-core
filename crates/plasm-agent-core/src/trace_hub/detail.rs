//! List/detail DTO projection and tenant visibility for trace reads.

use plasm_trace::{totals_from_session_data, SessionTraceData};
use uuid::Uuid;

use super::state::{ActiveTrace, CompletedTrace};
use super::{TraceDetailDto, TraceHub, TraceListStatus, TraceSummaryDto};

pub(super) fn tenant_visible_to_viewer(viewer_tenant_id: Option<&str>, trace_tenant: &str) -> bool {
    match viewer_tenant_id {
        None | Some("") => trace_tenant.is_empty() || trace_tenant == "anonymous",
        Some(v) => trace_tenant == v,
    }
}

impl TraceHub {
    pub(super) fn summary_dto_from_active(a: &ActiveTrace) -> TraceSummaryDto {
        TraceSummaryDto {
            trace_id: a.trace_id.to_string(),
            mcp_session_id: a.mcp_transport_session_id.clone().unwrap_or_default(),
            logical_session_id: a.logical_session_id.clone(),
            status: "live",
            started_at_ms: a.started_ms,
            ended_at_ms: None,
            project_slug: a.meta.project_slug.clone(),
            tenant_id: a.meta.tenant_id.clone(),
            mcp_config: a.meta.mcp_config.clone(),
            totals: totals_from_session_data(&a.data),
        }
    }

    fn detail_dto_from_active(a: &ActiveTrace) -> Option<TraceDetailDto> {
        let summary = Self::summary_dto_from_active(a);
        Some(Self::detail_dto_from_session(summary, &a.data))
    }

    pub(super) fn completed_to_summary(c: &CompletedTrace) -> TraceSummaryDto {
        TraceSummaryDto {
            trace_id: c.trace_id.to_string(),
            mcp_session_id: c.mcp_transport_session_id.clone().unwrap_or_default(),
            logical_session_id: c.logical_session_id.clone(),
            status: "completed",
            started_at_ms: c.started_ms,
            ended_at_ms: Some(c.ended_ms),
            project_slug: c.meta.project_slug.clone(),
            tenant_id: c.meta.tenant_id.clone(),
            mcp_config: c.meta.mcp_config.clone(),
            totals: totals_from_session_data(&c.data),
        }
    }

    pub(super) fn completed_to_detail(c: &CompletedTrace) -> TraceDetailDto {
        let summary = Self::completed_to_summary(c);
        Self::detail_dto_from_session(summary, &c.data)
    }

    fn detail_dto_from_session(
        summary: TraceSummaryDto,
        data: &SessionTraceData,
    ) -> TraceDetailDto {
        let records: Vec<serde_json::Value> = data
            .records
            .iter()
            .filter_map(|r| serde_json::to_value(r).ok())
            .collect();
        TraceDetailDto { summary, records }
    }

    /// List traces visible to this tenant (incoming-auth tenant id, or `"anonymous"` bucket).
    pub async fn list_for_tenant(
        &self,
        viewer_tenant_id: Option<&str>,
        project_slug: Option<&str>,
        offset: usize,
        limit: usize,
        status: TraceListStatus,
    ) -> Vec<TraceSummaryDto> {
        let lim = limit.clamp(1, 200);

        let tenant_ok = |t: &str| tenant_visible_to_viewer(viewer_tenant_id, t);
        let project_ok = |p: &str| match project_slug {
            None | Some("") => p == "main" || p.is_empty(),
            Some(want) => p == want,
        };
        let (active, completed) = {
            let g = self.inner.read().await;
            let active: Vec<ActiveTrace> = g
                .active
                .values()
                .filter(|a| tenant_ok(&a.meta.tenant_id) && project_ok(&a.meta.project_slug))
                .cloned()
                .collect();
            let completed: Vec<CompletedTrace> = g
                .completed
                .iter()
                .filter(|c| tenant_ok(&c.meta.tenant_id) && project_ok(&c.meta.project_slug))
                .cloned()
                .collect();
            (active, completed)
        };
        let mut out: Vec<TraceSummaryDto> = Vec::new();

        for a in &active {
            let st = "live";
            match status {
                TraceListStatus::All => {}
                TraceListStatus::Live if st != "live" => continue,
                TraceListStatus::Completed if st != "completed" => continue,
                _ => {}
            }
            out.push(Self::summary_dto_from_active(a));
        }
        for c in completed.iter().rev() {
            if status == TraceListStatus::Live {
                continue;
            }
            out.push(Self::completed_to_summary(c));
        }
        out.sort_by_key(|t| std::cmp::Reverse(t.started_at_ms));
        out.into_iter().skip(offset).take(lim).collect()
    }

    pub async fn get_detail(
        &self,
        trace_id: Uuid,
        viewer_tenant_id: Option<&str>,
    ) -> Option<TraceDetailDto> {
        let tenant_ok = |t: &str| tenant_visible_to_viewer(viewer_tenant_id, t);
        let selected = {
            let g = self.inner.read().await;
            if let Some(a) = g
                .active
                .values()
                .find(|a| a.trace_id == trace_id && tenant_ok(&a.meta.tenant_id))
                .cloned()
            {
                return Self::detail_dto_from_active(&a);
            }
            g.completed
                .iter()
                .find(|c| c.trace_id == trace_id && tenant_ok(&c.meta.tenant_id))
                .cloned()
        };
        selected.map(|c| Self::completed_to_detail(&c))
    }
}
