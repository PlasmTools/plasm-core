//! Keys tab

use super::super::*;

pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    layout_body: ratatui::layout::Rect,
    model: &mut RunState,
    _host_state: &PlasmHostState,
    _listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    let snap = &model.resources.snapshot;
    let (content_area, notice_area) = split_main_notice_area(layout_body, model.notice.is_some());
    let [keys_col, detail_col] = chrome::split_list_detail(content_area, 42);
    let key_inner_cols = keys_col.width.saturating_sub(2).max(1);
    let items: Vec<ListItem> = snap
        .keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let style = if i == model.keys.selected {
                selected_row_style()
            } else {
                Style::default()
            };
            let label = clip_list_row_plain(&api_key_row_label(k), key_inner_cols);
            ListItem::new(Line::from(vec![Span::styled(label, style)]))
        })
        .collect();
    frame.render_widget(
        List::new(items).block(chrome::panel_block("Keys", Some('k'))),
        keys_col,
    );
    let mut detail: Vec<Line> = vec![
        Line::from(vec![Span::styled(
            "Actions",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
    ];
    if let Some(buf) = model.add_key_label_buf() {
        detail.push(Line::from(vec![Span::styled(
            format!("New key label: {buf}_"),
            Style::default().add_modifier(Modifier::BOLD),
        )]));
        detail.push(Line::from(vec![
            Span::styled("Enter ", dim_style()),
            Span::raw("confirm · "),
            Span::styled("Esc ", dim_style()),
            Span::raw("cancel · "),
            Span::styled("^C ", dim_style()),
            Span::raw("quit appliance"),
        ]));
    } else {
        detail.push(Line::from(
            "Select a transport key, then use the footer shortcuts.",
        ));
        detail.push(Line::from(""));
        if let Some(k) = snap.keys.get(model.keys.selected) {
            detail.push(Line::from(vec![
                Span::styled("Label: ", dim_style()),
                Span::styled(
                    api_key_row_label(k),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]));
            detail.push(Line::from(vec![
                Span::styled("Fingerprint: ", dim_style()),
                Span::raw(k.fingerprint.clone()),
            ]));
            detail.push(Line::from(vec![
                Span::styled("Secret: ", dim_style()),
                Span::raw("use c on this tab (masked by default)."),
            ]));
        } else {
            detail.push(Line::from(Span::styled("No keys loaded yet.", dim_style())));
        }
    }
    frame.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: true })
            .block(chrome::panel_block("Detail", Some('a'))),
        detail_col,
    );
    if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
        render_notice_panel(frame, area, notice);
    }
}
