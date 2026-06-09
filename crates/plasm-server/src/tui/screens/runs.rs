//! Runs tab

use super::super::*;

pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    layout_body: ratatui::layout::Rect,
    model: &mut RunState,
    _host_state: &PlasmHostState,
    _listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    let (content_area, notice_area) = split_main_notice_area(layout_body, model.notice.is_some());
    let lines = vec![
        Line::from("Runs / traces"),
        Line::from(""),
        Line::from("Operational drill-down binds to execute session store and trace hub."),
        Line::from("Strict remote client: plasm (transport-only)."),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(chrome::panel_block("Runs", Some('r'))),
        content_area,
    );
    if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
        render_notice_panel(frame, area, notice);
    }
}
