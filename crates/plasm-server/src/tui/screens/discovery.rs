//! Discovery tab — semantic auto-seed settings.

use super::super::*;

pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    layout_body: ratatui::layout::Rect,
    model: &mut RunState,
    _host_state: &PlasmHostState,
    _listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    let (content_area, notice_area) = split_main_notice_area(layout_body, model.notice.is_some());
    let state = crate::discovery_bootstrap::current_state();
    let mut lines: Vec<Line> = vec![
        Line::from(vec![Span::styled(
            "Semantic auto-seed",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
    ];
    for text in crate::discovery_bootstrap::status_lines(&state) {
        lines.push(Line::from(text));
    }
    if let InputMode::DiscoveryOpenRouterKey { buf } = &model.mode {
        let mut display = buf.clone();
        display.push('_');
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("OpenRouter key: ", dim_style()),
            Span::raw("*".repeat(display.len().min(48))),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Enter to save · Esc cancel", dim_style()),
        ]));
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("e", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" toggle enable · "),
            Span::styled("k", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" set OpenRouter key"),
        ]));
    }
    render_scrollable_panel(frame, content_area, &lines, 0, "Discovery", Some('d'));
    if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
        render_notice_panel(frame, area, notice);
    }
}
