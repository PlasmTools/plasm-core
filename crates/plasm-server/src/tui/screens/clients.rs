//! Clients tab

use super::super::*;

pub(crate) fn build_clients_panel_lines(
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    selected_key: Option<&McpConfigApiKeyRow>,
) -> Vec<Line<'static>> {
    let accent = if no_color() {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .add_modifier(Modifier::BOLD)
            .fg(Color::Cyan)
    };
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let section = Style::default().add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
    let mut lines = Vec::new();
    if let Some(sel) = selected_key {
        lines.push(Line::from(vec![
            Span::styled("Key: ", bold),
            Span::styled(api_key_row_label(sel), accent),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Press ", dim_style()),
            Span::styled("c", dim_style().add_modifier(Modifier::BOLD)),
            Span::styled(" MCP config · ", dim_style()),
            Span::styled("p", dim_style().add_modifier(Modifier::BOLD)),
            Span::styled(" plasm CLI profile (with API key)", dim_style()),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("No keys yet", dim_style()),
            Span::raw(" — add one on the "),
            Span::styled("Keys", bold),
            Span::raw(" tab."),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("MCP client", section)));
    match mcp_client_json_config(listen, None) {
        Ok(json) => push_json_block_lines(&mut lines, &json),
        Err(e) => lines.push(Line::from(vec![
            Span::styled("! ", err_emphasis_style()),
            Span::styled(
                format!("Could not build MCP JSON: {e}"),
                err_emphasis_style(),
            ),
        ])),
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("Plasm CLI (plasm)", section)));
    lines.push(Line::from(Span::styled(
        plasm_cli_init_command_line(listen, None),
        dim_style(),
    )));
    lines.push(Line::from(""));
    match plasm_cli_profile_json_config(listen, None) {
        Ok(json) => push_json_block_lines(&mut lines, &json),
        Err(e) => lines.push(Line::from(vec![
            Span::styled("! ", err_emphasis_style()),
            Span::styled(
                format!("Could not build CLI profile JSON: {e}"),
                err_emphasis_style(),
            ),
        ])),
    }
    lines
}
pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    layout_body: ratatui::layout::Rect,
    model: &mut RunState,
    _host_state: &PlasmHostState,
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    let snap = &model.resources.snapshot;
    let (content_area, notice_area) = split_main_notice_area(layout_body, model.notice.is_some());
    let lines = build_clients_panel_lines(listen, snap.keys.get(model.keys.selected));
    render_scrollable_panel(
        frame,
        content_area,
        &lines,
        model.clients.scroll,
        "Clients",
        Some('e'),
    );
    if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
        render_notice_panel(frame, area, notice);
    }
}
