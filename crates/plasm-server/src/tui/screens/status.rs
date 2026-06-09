//! Status tab

use super::super::*;

pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    layout_body: ratatui::layout::Rect,
    model: &mut RunState,
    host_state: &PlasmHostState,
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    let snap = &model.resources.snapshot;
    let (content_area, notice_area) = split_main_notice_area(layout_body, model.notice.is_some());
    let lines = build_overview_lines(model, snap, host_state, listen);
    render_overview_panel(frame, content_area, &lines, model.overview.scroll);
    if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
        render_notice_panel(frame, area, notice);
    }
}
