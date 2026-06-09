//! Storage tab

use super::super::*;

pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    layout_body: ratatui::layout::Rect,
    model: &mut RunState,
    _host_state: &PlasmHostState,
    _listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    let (content_area, notice_area) = split_main_notice_area(layout_body, model.notice.is_some());
    let (backend_label, backend_detail) = storage_backend_summary(
        plasm_agent::embedded_postgres::EmbeddedPostgresGuard::will_autostart_embedded_postgres(),
        plasm_agent::embedded_postgres::EmbeddedPostgresGuard::embedded_autostart_skip_reason(),
    );
    let lines = vec![
                Line::from(vec![Span::styled(
                    "Backend",
                    Style::default().add_modifier(Modifier::BOLD),
                )]),
                Line::from(format!("  {backend_label}")),
                Line::from(format!("  {backend_detail}")),
                Line::from(""),
                Line::from(vec![Span::styled(
                    "Local files",
                    Style::default().add_modifier(Modifier::BOLD),
                )]),
                Line::from(format!("  Postgres data: {}", storage_postgres_data_dir())),
                Line::from(format!("  Local state:   {}", storage_local_state_dir())),
                Line::from(format!("  Auth KV key:   {}", storage_auth_key_path())),
                Line::from(""),
                Line::from(vec![Span::styled(
                    "Change it",
                    Style::default().add_modifier(Modifier::BOLD),
                )]),
                Line::from(
                    "  Use --data-dir <dir> to keep Postgres and local state in one predictable place.",
                ),
                Line::from(
                    "  Use PLASM_EMBEDDED_POSTGRES=0 plus DATABASE_URL=postgres://... to switch to an external database.",
                ),
            ];
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(chrome::panel_block("Storage", Some('s'))),
        content_area,
    );
    if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
        render_notice_panel(frame, area, notice);
    }
}
