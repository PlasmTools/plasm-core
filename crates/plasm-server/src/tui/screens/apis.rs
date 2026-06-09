//! Apis tab

use super::super::*;

pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    layout_body: ratatui::layout::Rect,
    model: &mut RunState,
    _host_state: &PlasmHostState,
    _listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    let snap = &model.resources.snapshot;
    let [list_col, right_col] = chrome::split_list_detail(layout_body, 46);
    let list_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(list_col);
    let filter_editing = matches!(model.mode, InputMode::ApiFilter);
    let mut filter_val = model.api.filter.clone();
    if filter_editing {
        filter_val.push('_');
    }
    frame.render_widget(
        Paragraph::new(chrome::filter_bar_line(
            "Filter catalogues (/)",
            filter_val.as_str(),
            filter_editing,
        )),
        list_rows[0],
    );
    let list_inner_cols = list_rows[1].width.saturating_sub(2).max(1);
    let mut lines: Vec<Line> = Vec::new();
    for (fi, &row_ix) in model.api.filtered_ix.iter().enumerate() {
        let r = &snap.catalog_rows[row_ix];
        let on = row_enabled(model, snap, &r.entry_id);
        let selected = fi == model.api.selected;
        let name = catalog_row_display_name(&r.entry_id, &r.label);
        let status = current_auth_config_label(r, snap);
        let summary = format!("{} · {}", auth_kind_label(r), status);
        lines.push(format_api_catalogue_row(
            selected,
            on,
            &name,
            &summary,
            list_inner_cols,
        ));
    }
    frame.render_widget(
        Paragraph::new(lines).block(chrome::panel_block("Catalogues", Some('l'))),
        list_rows[1],
    );

    let (detail_area, notice_area) = split_main_notice_area(right_col, model.notice.is_some());
    let mut detail_lines = vec![
        Line::from(vec![Span::styled(
            "Selected catalogue",
            Style::default().add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
    ];
    if let Some(row) = selected_api_row(model, snap) {
        detail_lines.push(Line::from(vec![
            Span::styled("Entry: ", dim_style()),
            Span::styled(
                row.entry_id.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        detail_lines.push(Line::from(vec![
            Span::styled("Supported auth: ", dim_style()),
            Span::raw(auth_kind_label(row)),
        ]));
        detail_lines.push(Line::from(vec![
            Span::styled("Current config: ", dim_style()),
            Span::raw(current_auth_config_label(row, snap)),
        ]));
        detail_lines.push(Line::from(vec![
            Span::styled("Scheme: ", dim_style()),
            Span::raw(if row.auth_scheme_summary.is_empty() {
                "public".to_string()
            } else {
                row.auth_scheme_summary.clone()
            }),
        ]));
        detail_lines.push(Line::from(vec![
            Span::styled("Allowlist: ", dim_style()),
            Span::raw(if row_enabled(model, snap, &row.entry_id) {
                "enabled"
            } else {
                "disabled"
            }),
        ]));
        if let Some(oauth) = oauth_provider_summary(snap, &row.entry_id) {
            detail_lines.push(Line::from(vec![
                Span::styled("OAuth app: ", dim_style()),
                Span::raw(oauth),
            ]));
        } else if row.connect_profile.has_oauth {
            detail_lines.push(Line::from(vec![
                Span::styled("OAuth app: ", dim_style()),
                Span::raw("not configured"),
            ]));
        }
        if row.api_secret_present {
            detail_lines.push(Line::from(vec![
                Span::styled("Secret: ", dim_style()),
                Span::raw("stored"),
            ]));
        } else if row.connect_profile.has_api_key {
            detail_lines.push(Line::from(vec![
                Span::styled("Secret: ", dim_style()),
                Span::raw("missing"),
            ]));
        }
        if row.bindings_required {
            detail_lines.push(Line::from(vec![
                Span::styled("Bindings: ", dim_style()),
                Span::raw(if row.bindings_complete {
                    "complete"
                } else {
                    "missing (catalog_http_origin)"
                }),
            ]));
        }
        if let Some(ref key) = row.api_secret_hosted_kv {
            detail_lines.push(Line::from(vec![
                Span::styled("Hosted key: ", dim_style()),
                Span::raw(key.clone()),
            ]));
        }
        if let InputMode::ApiSecretEdit { entry_id, buf, .. } = &model.mode {
            if entry_id == &row.entry_id {
                detail_lines.push(Line::from(""));
                detail_lines.push(Line::from(vec![
                    Span::styled("New API key secret: ", dim_style()),
                    Span::styled(
                        "*".repeat(buf.len()) + "_",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ]));
                detail_lines.push(Line::from(
                    "Enter save · Esc cancel · secret is masked in this pane only",
                ));
            }
        }
        if let InputMode::CatalogConnect {
            entry_id,
            step,
            workspace_url,
            secret_buf,
            ..
        } = &model.mode
        {
            if entry_id == &row.entry_id {
                detail_lines.push(Line::from(""));
                if *step == 0 {
                    detail_lines.push(Line::from(vec![
                        Span::styled("Workspace URL: ", dim_style()),
                        Span::raw(workspace_url.clone()),
                    ]));
                } else {
                    detail_lines.push(Line::from(vec![
                        Span::styled("Workspace URL: ", dim_style()),
                        Span::raw(workspace_url.clone()),
                    ]));
                    detail_lines.push(Line::from(vec![
                        Span::styled("API key: ", dim_style()),
                        Span::styled(
                            "*".repeat(secret_buf.len()) + "_",
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
                detail_lines.push(Line::from("Enter next/save · Esc cancel"));
            }
        }
    } else {
        detail_lines.push(Line::from("No catalogue selected."));
    }
    frame.render_widget(
        Paragraph::new(detail_lines)
            .wrap(Wrap { trim: true })
            .block(chrome::panel_block("Details", Some('d'))),
        detail_area,
    );
    if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
        render_notice_panel(frame, area, notice);
    }
}
