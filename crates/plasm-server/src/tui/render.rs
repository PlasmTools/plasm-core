//! Main frame render and scrollable panels.

use super::screens;
use super::*;

pub(crate) fn clamp_overview_scroll(scroll: u16, line_count: usize, visible: usize) -> u16 {
    if line_count == 0 || visible == 0 {
        return 0;
    }
    let max_top = line_count.saturating_sub(visible);
    scroll.min(max_top as u16)
}

pub(crate) fn build_overview_lines(
    model: &RunState,
    snap: &UiSnapshot,
    host_state: &PlasmHostState,
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) -> Vec<Line<'static>> {
    let scope = appliance_mcp_scope();
    let mut lines = vec![
        Line::from("Listeners"),
        Line::from(format!(
            "  HTTP+MCP   {}  (MCP: /mcp)",
            listen.client_mcp_streamable_url()
        )),
        Line::from(format!("  bind       {}", listen.display_addr())),
    ];
    if let Some(hint) = listen.local_client_hint_line() {
        lines.push(Line::from(hint));
    }
    lines.push(Line::from(""));
    lines.push(Line::from("Your MCP (singleton)"));
    match &snap.config_surface {
        McpConfigSurfaceState::Ready {
            summary_name,
            summary_status,
            enabled_api_count,
            key_count,
        } => {
            lines.push(Line::from("  policy store (project_mcp_*): enabled"));
            lines.push(Line::from(format!(
                "  tenant / workspace / project: {} / {} / {}",
                scope.tenant_id, scope.workspace_slug, scope.project_slug
            )));
            lines.push(Line::from(format!(
                "  config: {}  ({})",
                summary_name, summary_status
            )));
            lines.push(Line::from(format!(
                "  enabled APIs: {}  transport keys: {}",
                enabled_api_count, key_count
            )));
            if let Some(id) = model.resources.config_id {
                lines.push(Line::from(format!("  config_id: {id}")));
            }
        }
        McpConfigSurfaceState::ConfigLoadError => {
            lines.push(Line::from(vec![
                Span::styled("  ! ", err_emphasis_style()),
                Span::styled(
                    "MCP policy store online, but the singleton config failed to load.",
                    err_emphasis_style(),
                ),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  > ", dim_style()),
                Span::raw("Wait for refresh or inspect startup / DB diagnostics."),
            ]));
        }
        McpConfigSurfaceState::PolicyStoreUnavailable { reason } => match reason {
            PolicyStoreUnavailableReason::RefreshPending => {
                lines.push(Line::from(Span::styled(
                    "  policy store (project_mcp_*): refreshing…",
                    dim_style(),
                )));
            }
            PolicyStoreUnavailableReason::NeverAttached => {
                lines.push(Line::from(Span::styled(
                    "  ! ERROR: MCP policy store offline",
                    err_emphasis_style(),
                )));
                lines.push(Line::from(Span::styled(
                    "  ! project_mcp_* not reachable (database missing or migrations failed).",
                    err_emphasis_style(),
                )));
                if let Some(detail) = model.policy_bootstrap_detail.as_ref() {
                    lines.push(Line::from(""));
                    for line in detail.display_lines() {
                        lines.push(Line::from(Span::styled(format!("  > {line}"), dim_style())));
                    }
                }
                lines.push(Line::from(Span::styled(
                    "  x Transport API keys and API allowlists are disabled until fixed.",
                    err_emphasis_style(),
                )));
                lines.push(Line::from(""));
                lines.push(Line::from(
                    "  > Fix: wipe ~/.plasm/appliance/postgres and restart, or run: plasm-server mcp migrate-db",
                ));
                lines.push(Line::from("  > See Logs tab for bootstrap / sqlx details."));
            }
        },
    }
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "Trace hub: {}",
        plasm_agent_core::appliance_services::trace_hub_bounds_summary(host_state)
    )));
    lines
}

pub(crate) fn render_scrollable_panel(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    lines: &[Line<'static>],
    scroll: u16,
    title: &str,
    title_hotkey: Option<char>,
) {
    let visible = area.height.saturating_sub(2) as usize;
    let scroll = clamp_overview_scroll(scroll, lines.len(), visible);
    frame.render_widget(
        Paragraph::new(lines.to_vec())
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false })
            .block(chrome::panel_block(title, title_hotkey)),
        area,
    );
}

pub(crate) fn render_overview_panel(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    lines: &[Line<'static>],
    scroll: u16,
) {
    render_scrollable_panel(frame, area, lines, scroll, "Overview", Some('o'));
}

pub(crate) fn render_running_frame(
    frame: &mut ratatui::Frame<'_>,
    model: &mut RunState,
    host_state: &PlasmHostState,
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    chrome::clear_frame(frame);
    let layout = chrome::split_running_vertical(frame.area());
    let tab_titles: Vec<&str> = RunScreen::ALL.iter().map(|s| s.title()).collect();
    let rail_max = layout.tab_rail.width.max(1);
    let rail = chrome::tab_rail_line(model.screen.index(), &tab_titles, listen, rail_max);
    chrome::render_tab_rail(frame, layout.tab_rail, rail);

    match model.screen {
        RunScreen::Status => screens::status::render(frame, layout.body, model, host_state, listen),
        RunScreen::Clients => {
            screens::clients::render(frame, layout.body, model, host_state, listen)
        }
        RunScreen::Apis => screens::apis::render(frame, layout.body, model, host_state, listen),
        RunScreen::OAuth => screens::oauth::render(frame, layout.body, model, host_state, listen),
        RunScreen::Keys => screens::keys::render(frame, layout.body, model, host_state, listen),
        RunScreen::Runs => screens::runs::render(frame, layout.body, model, host_state, listen),
        RunScreen::Storage => {
            screens::storage::render(frame, layout.body, model, host_state, listen)
        }
        RunScreen::Logs => screens::logs::render(frame, layout.body, model, host_state, listen),
    }

    let global = [
        chrome::FooterItem::new("←/→", "tab"),
        chrome::FooterItem::new("Tab", "next"),
        chrome::FooterItem::new("q", "quit"),
    ];
    let screen_items = screen_footer_items(model);
    let mode_l = input_mode_label(&model.mode);
    let admin = model.resources.admin.busy_task().map(|t| {
        format!(
            "{} {:.0}s",
            t.kind.label(),
            t.started_at.elapsed().as_secs_f32()
        )
    });
    let footer_line = chrome::footer_line(&global, &screen_items, mode_l, admin.as_deref());
    chrome::render_footer_bar(frame, layout.footer, footer_line);
}
