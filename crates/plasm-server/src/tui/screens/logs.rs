//! Logs tab

use super::super::*;

pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    layout_body: ratatui::layout::Rect,
    model: &mut RunState,
    _host_state: &PlasmHostState,
    _listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    let (content_area, notice_area) = split_main_notice_area(layout_body, model.notice.is_some());
    let [log_col, detail_col] = chrome::split_list_detail(content_area, 44);
    let list_inner_h = log_col.height.saturating_sub(2) as usize;
    let visible_rows = list_inner_h.max(1);
    sync_log_cursor_scroll(&mut model.logs, visible_rows);
    let total = model.logs.lines.len();
    let max_top = total.saturating_sub(visible_rows.min(total.max(1)));
    let top = model.logs.scroll.min(max_top);
    let inner_w = log_col.width.saturating_sub(2).max(1);
    let clip_cols = inner_w.saturating_sub(2).max(1);
    let items: Vec<ListItem> = model
        .logs
        .lines
        .iter()
        .enumerate()
        .skip(top)
        .take(visible_rows)
        .map(|(gi, entry)| {
            let selected = gi == model.logs.cursor;
            let row_style = if selected {
                selected_row_style()
            } else {
                log_render::log_list_unselected_style()
            };
            let line = log_render::format_list_line(entry, selected, row_style, clip_cols);
            ListItem::new(line)
        })
        .collect();
    frame.render_widget(
        List::new(items).block(chrome::panel_block("Log", Some('l'))),
        log_col,
    );
    let detail_lines = model
        .logs
        .lines
        .get(model.logs.cursor)
        .map(log_render::format_detail_lines)
        .unwrap_or_else(|| vec![Line::from("(no log line selected)")]);
    let detail_block = chrome::panel_block("Line", Some('d')).style(Style::default());
    frame.render_widget(
        Paragraph::new(detail_lines)
            .wrap(Wrap { trim: true })
            .block(detail_block),
        detail_col,
    );
    if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
        render_notice_panel(frame, area, notice);
    }
}
