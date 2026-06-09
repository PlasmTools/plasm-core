//! Oauth tab

use super::super::*;

pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    layout_body: ratatui::layout::Rect,
    model: &mut RunState,
    _host_state: &PlasmHostState,
    _listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    let snap = &model.resources.snapshot;
    if let InputMode::OAuthWizard(ref wiz) = model.mode {
        let (content_area, notice_area) =
            split_main_notice_area(layout_body, model.notice.is_some());
        let mut lines = vec![
            Line::from(vec![
                Span::styled(
                    "New OAuth provider (upsert) ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled("Esc cancel", dim_style()),
                Span::raw(" · "),
                Span::styled("Enter", dim_style()),
                Span::raw(" next / confirm"),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Field: ", dim_style()),
                Span::styled(
                    wiz.prompt_title(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
        ];
        match wiz.step {
            OAuthUpsertStep::EntryId => {
                let mut search = wiz.buf.clone();
                search.push('_');
                lines.push(Line::from(vec![
                    Span::styled("Search: ", dim_style()),
                    Span::styled(search, Style::default().add_modifier(Modifier::BOLD)),
                ]));
                lines.push(Line::from(Span::styled(
                    "Type to filter · ↑↓ or j/k choose · Enter selects",
                    dim_style(),
                )));
                lines.push(Line::from(""));
                let matches = wiz.filtered_entry_indices(&snap.catalog_rows);
                if matches.is_empty() {
                    lines.push(Line::from(Span::styled(
                        "(No registry API matches the current search.)",
                        dim_style(),
                    )));
                } else {
                    let selected = wiz.entry_sel.min(matches.len().saturating_sub(1));
                    let start = selected.saturating_sub(4);
                    let end = (start + 8).min(matches.len());
                    let start = end.saturating_sub(8);
                    if start > 0 {
                        lines.push(Line::from(Span::styled(
                            format!("… {} earlier matches", start),
                            dim_style(),
                        )));
                    }
                    for (offset, row_ix) in matches[start..end].iter().enumerate() {
                        let absolute = start + offset;
                        let row = &snap.catalog_rows[*row_ix];
                        let picked = absolute == selected;
                        let mut row_style = Style::default();
                        let mut meta_style = dim_style();
                        if picked {
                            row_style = selected_row_style();
                            meta_style = selected_row_style();
                        }
                        lines.push(Line::from(vec![
                            Span::styled(if picked { "› " } else { "  " }, row_style),
                            Span::styled(
                                catalog_row_display_name(&row.entry_id, &row.label),
                                row_style,
                            ),
                            Span::raw("  "),
                            Span::styled(format!("{:?}", row.auth_marker), meta_style),
                        ]));
                    }
                    if end < matches.len() {
                        lines.push(Line::from(Span::styled(
                            format!("… {} more matches", matches.len() - end),
                            dim_style(),
                        )));
                    }
                }
            }
            OAuthUpsertStep::Enabled => {
                lines.push(Line::from(vec![
                    Span::raw("enabled: "),
                    Span::styled(
                        if wiz.enabled { "yes" } else { "no" },
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled("  — Space toggles, Enter review", dim_style()),
                ]));
            }
            OAuthUpsertStep::Confirm => {
                lines.push(Line::from(vec![Span::styled(
                    "Review — Enter save, Esc cancel wizard",
                    dim_style(),
                )]));
                lines.push(Line::from(""));
                for s in wiz.summary_lines() {
                    lines.push(Line::from(Span::raw(s)));
                }
            }
            _ => {
                let mut edit = wiz.buf.clone();
                edit.push('_');
                lines.push(Line::from(vec![Span::styled(
                    edit,
                    Style::default().add_modifier(Modifier::BOLD),
                )]));
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(chrome::panel_block("OAuth wizard", Some('w'))),
            content_area,
        );
        if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
            render_notice_panel(frame, area, notice);
        }
    } else if let InputMode::OAuthDeviceScopePick(ref pick) = model.mode {
        let (content_area, notice_area) =
            split_main_notice_area(layout_body, model.notice.is_some());
        let mut lines = vec![
                    Line::from(vec![
                        Span::styled(
                            "Device bind — catalogue OAuth scopes ",
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("Esc cancel", dim_style()),
                        Span::raw(" · "),
                        Span::styled("Enter", dim_style()),
                        Span::raw(" start device flow"),
                    ]),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("Catalogue: ", dim_style()),
                        Span::styled(
                            pick.entry_id.as_str(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(Span::styled(
                        "↑↓ / j k move · Space toggles · 1–9 applies a CGS default_scope_sets bundle (when listed).",
                        dim_style(),
                    )),
                    Line::from(""),
                ];
        for (i, (id, label)) in pick.scope_rows.iter().enumerate() {
            let cursor_here = i == pick.cursor;
            let on = pick.selected.contains(id);
            let mut row_style = Style::default();
            let mut meta = dim_style();
            if cursor_here {
                row_style = selected_row_style();
                meta = selected_row_style();
            }
            let mark = if on { "[x] " } else { "[ ] " };
            lines.push(Line::from(vec![
                Span::styled(if cursor_here { "› " } else { "  " }, row_style),
                Span::styled(mark, meta),
                Span::styled(id.as_str(), row_style),
                Span::raw(" — "),
                Span::styled(label.as_str(), meta),
            ]));
        }
        if !pick.default_sets.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![Span::styled(
                "CGS default_scope_sets (keys 1–9):",
                dim_style(),
            )]));
            for (ix, (name, scopes)) in pick.default_sets.iter().enumerate().take(9) {
                lines.push(Line::from(vec![
                    Span::styled(format!("  {}. ", ix + 1), dim_style()),
                    Span::raw(name.as_str()),
                    Span::styled(format!("  ({} scopes)", scopes.len()), dim_style()),
                ]));
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(chrome::panel_block("OAuth scopes", Some('o'))),
            content_area,
        );
        if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
            render_notice_panel(frame, area, notice);
        }
    } else {
        let [split0, split1] = chrome::split_list_detail(layout_body, 40);
        let provider_items: Vec<ListItem> = if snap.oauth_providers.is_empty() {
            vec![ListItem::new(Line::from(Span::styled(
                "No providers configured",
                dim_style(),
            )))]
        } else {
            let inner_cols = split0.width.saturating_sub(2).max(1);
            snap.oauth_providers
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let selected = i == model.oauth.selected;
                    let mut name_style = if selected {
                        selected_row_style()
                    } else {
                        Style::default()
                    };
                    if !row.enabled {
                        name_style = name_style.patch(api_toggle_off_style());
                    }
                    let binding_hint = snap
                        .oauth_binding_hints
                        .get(i)
                        .map(String::as_str)
                        .unwrap_or("binding unknown");
                    let plain = format!(
                        "{}  {}  {}  {}",
                        if selected { "›" } else { " " },
                        row.entry_id,
                        if row.enabled { "enabled" } else { "disabled" },
                        binding_hint
                    );
                    let clipped = clip_list_row_plain(&plain, inner_cols);
                    ListItem::new(Line::from(vec![Span::styled(clipped, name_style)]))
                })
                .collect()
        };
        frame.render_widget(
            List::new(provider_items).block(chrome::panel_block("Providers", Some('p'))),
            split0,
        );

        let (detail_area, notice_area) = split_main_notice_area(split1, model.notice.is_some());
        let mut lines = vec![
            Line::from(vec![Span::styled(
                "Binding",
                Style::default().add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Tip: ", dim_style()),
                Span::styled(
                    "plasm-server oauth",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(" for scripting / secrets via stdin.", dim_style()),
            ]),
            Line::from(""),
        ];
        if let Some(row) = snap.oauth_providers.get(model.oauth.selected) {
            let binding_hint = snap
                .oauth_binding_hints
                .get(model.oauth.selected)
                .map(String::as_str)
                .unwrap_or("binding unknown");
            lines.push(Line::from(vec![
                Span::styled("Provider: ", dim_style()),
                Span::styled(
                    row.entry_id.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]));
            lines.push(Line::from(format!(
                "Enabled: {}",
                if row.enabled { "yes" } else { "no" }
            )));
            lines.push(Line::from(format!("Binding: {binding_hint}")));
            let device_ep = row
                .device_authorization_endpoint
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            lines.push(Line::from(format!(
                "Device authorization: {}",
                if device_ep.is_some() {
                    "available"
                } else {
                    "not configured"
                }
            )));
            if let Some(device_ep) = device_ep {
                lines.push(Line::from(format!("Device endpoint: {device_ep}")));
            }
        } else if snap.oauth_surface.provider_store_ready() {
            lines.push(Line::from(vec![
                Span::styled("No providers configured. ", dim_style()),
                Span::raw("Press "),
                Span::styled("n", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" to add one."),
            ]));
        }
        if let Some(status) = oauth_surface_status(snap) {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("OAuth status: ", err_emphasis_style()),
                Span::styled(status, err_emphasis_style()),
            ]));
        }
        if let Some(entry_id) = model.pending_oauth_disable_entry() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("Disable pending: ", warn_emphasis_style()),
                Span::styled(entry_id.to_string(), warn_emphasis_style()),
            ]));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .block(chrome::panel_block("Details", Some('d'))),
            detail_area,
        );
        if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
            render_notice_panel(frame, area, notice);
        }
    }
}
