//! Ratatui control station — Your MCP–oriented tabs over [`PlasmHostState`] (no loopback HTTP).

use std::collections::{HashSet, VecDeque};
use std::io::{self, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use plasm_agent_core::mcp_config_admin::{
    McpCatalogAuthMarker, McpConfigApiKeyRow, McpConfigCatalogRow,
};
use plasm_agent_core::server_state::PlasmHostState;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Terminal;
use uuid::Uuid;

use crate::appliance_admin_bridge::{
    config_surface_from_host, AdminBridge, AdminCompletion, AdminCorr, AdminJob,
    McpConfigSurfaceState, OAuthSurfaceState, PolicyStoreUnavailableReason, RefreshedUiData,
};
use crate::appliance_log;
use crate::appliance_mcp_admin::appliance_mcp_scope;
use crate::appliance_mode::PolicyStoreBootstrapDetail;
use crate::boot::UiEvent;
use crate::oauth_upsert_wizard::{OAuthUpsertStep, OAuthUpsertWizard};

mod chrome;
mod log_render;
mod oauth_device_scope_pick;

use oauth_device_scope_pick::OAuthDeviceScopePickState;

/// Raw TTY (`cfmakeraw`) does not raise SIGINT on ^C — the byte is delivered as input. Match that
/// here so Ctrl+C still exits the control station (Tokio `ctrl_c()` alone never fires in raw mode).
///
/// **tui-design note:** the TUI skill discourages binding terminal-owned chords (`Ctrl+C`, etc.).
/// This path is an intentional exception: without it, users cannot interrupt the alternate-screen
/// loop from the keyboard because no SIGINT reaches the Tokio handler. Primary quit remains `q`.
fn raw_tty_wants_process_quit(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('\x03'))
        || (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C')))
}

const MCP_JSON_PLACEHOLDER_BEARER: &str = "Bearer <api_key>";
const PLASM_CLI_PLACEHOLDER_API_KEY: &str = "<api_key>";

fn bearer_authorization_value(raw_secret: Option<&str>) -> String {
    match raw_secret {
        None => MCP_JSON_PLACEHOLDER_BEARER.to_string(),
        Some(raw) => {
            let t = raw.trim();
            if t.is_empty() {
                MCP_JSON_PLACEHOLDER_BEARER.to_string()
            } else if t.len() >= 7 && t[..7].eq_ignore_ascii_case("bearer ") {
                t.to_string()
            } else {
                format!("Bearer {t}")
            }
        }
    }
}

fn mcp_client_json_config(
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    raw_secret: Option<&str>,
) -> Result<String, String> {
    let auth = bearer_authorization_value(raw_secret);
    let value = serde_json::json!({
        "mcpServers": {
            "plasm": {
                "type": "streamableHttp",
                "url": listen.client_mcp_streamable_url(),
                "headers": {
                    "Authorization": auth
                }
            }
        }
    });
    serde_json::to_string_pretty(&value)
        .map(|s| format!("{s}\n"))
        .map_err(|e| e.to_string())
}

fn plasm_cli_api_key_value(raw_secret: Option<&str>) -> String {
    match raw_secret {
        None => PLASM_CLI_PLACEHOLDER_API_KEY.to_string(),
        Some(raw) => {
            let t = raw.trim();
            if t.is_empty() {
                PLASM_CLI_PLACEHOLDER_API_KEY.to_string()
            } else {
                t.to_string()
            }
        }
    }
}

fn plasm_cli_profile_json_config(
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    raw_secret: Option<&str>,
) -> Result<String, String> {
    let value = serde_json::json!({
        "server": listen.client_http_origin(),
        "api_key": plasm_cli_api_key_value(raw_secret),
    });
    serde_json::to_string_pretty(&value)
        .map(|s| format!("{s}\n"))
        .map_err(|e| e.to_string())
}

fn plasm_cli_init_command_line(
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    raw_secret: Option<&str>,
) -> String {
    format!(
        "plasm init --server {} --api-key {}",
        listen.client_http_origin(),
        plasm_cli_api_key_value(raw_secret)
    )
}

fn push_json_block_lines(lines: &mut Vec<Line<'static>>, json: &str) {
    for line in json.lines() {
        lines.push(Line::from(Span::styled(line.to_string(), dim_style())));
    }
}

fn build_clients_panel_lines(
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

fn api_key_row_label(k: &McpConfigApiKeyRow) -> String {
    match k.label.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => format!("(unnamed · fp:{})", fingerprint_head(&k.fingerprint)),
    }
}

fn fingerprint_head(fingerprint: &str) -> &str {
    let trimmed = fingerprint.trim();
    if trimmed.is_empty() {
        "unknown"
    } else {
        &trimmed[..trimmed.len().min(8)]
    }
}

fn api_key_row_copy_line(k: &McpConfigApiKeyRow) -> String {
    api_key_row_label(k)
}

fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text).map_err(|e| e.to_string())
}

fn env_nonempty_string(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn storage_backend_summary(
    embedded_autostart: bool,
    skip_reason: Option<&str>,
) -> (&'static str, String) {
    if embedded_autostart {
        (
            "Embedded Postgres",
            "This appliance is managing its own local PostgreSQL 15 cluster.".into(),
        )
    } else {
        (
            "External / disabled Postgres",
            skip_reason
                .unwrap_or("Embedded Postgres is not active for this appliance.")
                .to_string(),
        )
    }
}

fn storage_postgres_data_dir() -> String {
    env_nonempty_string("PLASM_EMBEDDED_POSTGRES_DATA_DIR")
        .or_else(|| env_nonempty_string("PGDATA"))
        .unwrap_or_else(|| "managed OS cache (use --data-dir to pin it)".into())
}

fn storage_local_state_dir() -> String {
    plasm_agent_core::oss_local_state::resolve_local_state_root()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "unavailable (HOME / PLASM_LOCAL_STATE_DIR unset)".into())
}

fn storage_auth_key_path() -> String {
    plasm_agent_core::oss_local_state::resolve_local_state_root()
        .map(|p| {
            p.join("bootstrap-secrets")
                .join("AUTH_STORAGE_ENCRYPTION_KEY")
                .display()
                .to_string()
        })
        .unwrap_or_else(|| "unavailable until local state root is known".into())
}

fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some()
}

fn run_title_style() -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD);
    if !no_color() {
        s = s.fg(Color::Cyan);
    }
    s
}

fn dim_style() -> Style {
    let mut s = Style::default();
    if !no_color() {
        s = s.fg(Color::DarkGray);
    } else {
        s = s.add_modifier(Modifier::DIM);
    }
    s
}

fn err_emphasis_style() -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD);
    if !no_color() {
        s = s.fg(Color::Red);
    }
    s
}

fn warn_emphasis_style() -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD);
    if !no_color() {
        s = s.fg(Color::Yellow);
    }
    s
}

fn api_toggle_on_style() -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD);
    if !no_color() {
        s = s.fg(Color::Green);
    }
    s
}

fn api_toggle_off_style() -> Style {
    let mut s = Style::default();
    if !no_color() {
        s = s.fg(Color::DarkGray);
    } else {
        s = s.add_modifier(Modifier::DIM);
    }
    s
}

fn selected_row_style() -> Style {
    let mut s = Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
    if !no_color() {
        s = s.fg(Color::Black).bg(Color::Yellow);
    }
    s
}

fn catalog_row_display_name(entry_id: &str, label: &str) -> String {
    if label.trim() == entry_id.trim() {
        entry_id.to_string()
    } else {
        format!("{entry_id} — {label}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoticeSeverity {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunNotice {
    severity: NoticeSeverity,
    title: String,
    summary: String,
    details: Vec<String>,
    action_hint: Option<String>,
    sticky: bool,
}

impl RunNotice {
    fn new(severity: NoticeSeverity, title: impl Into<String>, summary: impl Into<String>) -> Self {
        let sticky = matches!(severity, NoticeSeverity::Error);
        Self {
            severity,
            title: title.into(),
            summary: summary.into(),
            details: Vec::new(),
            action_hint: None,
            sticky,
        }
    }

    fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }

    fn with_action_hint(mut self, hint: impl Into<String>) -> Self {
        self.action_hint = Some(hint.into());
        self
    }

    fn with_sticky(mut self, sticky: bool) -> Self {
        self.sticky = sticky;
        self
    }

    fn severity_label(&self) -> &'static str {
        match self.severity {
            NoticeSeverity::Info => "INFO",
            NoticeSeverity::Success => "SUCCESS",
            NoticeSeverity::Warning => "WARNING",
            NoticeSeverity::Error => "ERROR",
        }
    }

    fn heading_style(&self) -> Style {
        match self.severity {
            NoticeSeverity::Info => run_title_style(),
            NoticeSeverity::Success => api_toggle_on_style(),
            NoticeSeverity::Warning => warn_emphasis_style(),
            NoticeSeverity::Error => err_emphasis_style(),
        }
    }

    fn block_title(&self) -> String {
        self.title.clone()
    }

    fn lines(&self) -> Vec<Line<'static>> {
        let mut lines = vec![Line::from(vec![
            Span::styled(format!("{} ", self.severity_label()), self.heading_style()),
            Span::styled(self.summary.clone(), self.heading_style()),
        ])];
        if !self.details.is_empty() {
            lines.push(Line::from(""));
            lines.extend(self.details.iter().cloned().map(Line::from));
        }
        if let Some(ref hint) = self.action_hint {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("Next: ", dim_style()),
                Span::raw(hint.clone()),
            ]));
        }
        lines
    }
}

fn set_notice(state: &mut RunState, notice: RunNotice) {
    state.notice = Some(notice);
}

fn dismiss_transient_notice(state: &mut RunState) -> bool {
    if state.notice.as_ref().is_some_and(|notice| !notice.sticky) {
        state.notice = None;
        return true;
    }
    false
}

fn split_main_notice_area(area: Rect, show_notice: bool) -> (Rect, Option<Rect>) {
    chrome::split_with_notice(area, show_notice)
}

fn sync_log_cursor_scroll(logs: &mut LogState, visible: usize) {
    let total = logs.lines.len();
    if total == 0 {
        logs.cursor = 0;
        logs.scroll = 0;
        return;
    }
    logs.cursor = logs.cursor.min(total.saturating_sub(1));
    let vis = visible.max(1).min(total);
    if logs.cursor < logs.scroll {
        logs.scroll = logs.cursor;
    }
    let bottom = logs.scroll.saturating_add(vis.saturating_sub(1));
    if logs.cursor > bottom {
        logs.scroll = logs.cursor.saturating_add(1).saturating_sub(vis);
    }
    let max_top = total.saturating_sub(vis);
    logs.scroll = logs.scroll.min(max_top);
}

fn screen_footer_items(model: &RunState) -> Vec<chrome::FooterItem> {
    use chrome::FooterItem;
    match model.screen {
        RunScreen::Status => vec![
            FooterItem::new("↑↓", "scroll"),
            FooterItem::new("PgUp/Dn", "page"),
        ],
        RunScreen::Clients => vec![
            FooterItem::new("c", "copy MCP config"),
            FooterItem::new("p", "copy plasm CLI profile"),
            FooterItem::new("#", "copy MCP URL"),
            FooterItem::new("↑↓", "scroll"),
        ],
        RunScreen::Apis => vec![
            FooterItem::new("/", "filter"),
            FooterItem::new("Space", "toggle"),
            FooterItem::new("s", "save allowlist"),
            FooterItem::new("a", "API key"),
            FooterItem::new("o", "OAuth"),
        ],
        RunScreen::OAuth => match &model.mode {
            InputMode::OAuthDeviceScopePick(_) => vec![
                FooterItem::new("↑↓/jk", "move"),
                FooterItem::new("Space", "toggle"),
                FooterItem::new("1-9", "bundle"),
                FooterItem::new("Enter", "device"),
                FooterItem::new("Esc", "cancel"),
            ],
            InputMode::OAuthWizard(_) => vec![
                FooterItem::new("Esc", "cancel"),
                FooterItem::new("Enter", "confirm"),
            ],
            _ => vec![
                FooterItem::new("n", "new provider"),
                FooterItem::new("d", "device bind"),
                FooterItem::new("x", "disable"),
                FooterItem::new("y", "confirm"),
            ],
        },
        RunScreen::Keys => {
            let mut v = vec![
                FooterItem::new("a", "add"),
                FooterItem::new("r", "rotate"),
                FooterItem::new("d", "revoke"),
                FooterItem::new("c", "copy secret"),
            ];
            if model.add_key_label_buf().is_none() {
                v.push(FooterItem::new("#", "copy label"));
            }
            v
        }
        RunScreen::Logs => vec![
            FooterItem::new("↑↓", "move"),
            FooterItem::new("PgUp/Dn", "page"),
            FooterItem::new("g/G", "top/bottom"),
        ],
        RunScreen::Runs | RunScreen::Storage => vec![],
    }
}

fn render_notice_panel(frame: &mut ratatui::Frame<'_>, area: Rect, notice: &RunNotice) {
    frame.render_widget(
        Paragraph::new(notice.lines())
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(notice.block_title()),
            ),
        area,
    );
}

fn selected_oauth_entry_id(state: &RunState) -> Option<&str> {
    state
        .resources
        .snapshot
        .oauth_providers
        .get(state.oauth.selected)
        .map(|row| row.entry_id.as_str())
}

fn selected_api_row<'a>(
    state: &'a RunState,
    snap: &'a UiSnapshot,
) -> Option<&'a McpConfigCatalogRow> {
    let row_ix = *state.api.filtered_ix.get(state.api.selected)?;
    snap.catalog_rows.get(row_ix)
}

fn auth_kind_label(row: &McpConfigCatalogRow) -> String {
    let mut kinds = Vec::new();
    if row.connect_profile.has_public_mode {
        kinds.push("public");
    }
    if row.connect_profile.has_api_key {
        kinds.push("api key");
    }
    if row.connect_profile.has_oauth {
        kinds.push("oauth");
    }
    if kinds.is_empty() {
        "public".into()
    } else {
        kinds.join("+")
    }
}

fn oauth_provider_summary(snap: &UiSnapshot, entry_id: &str) -> Option<String> {
    let idx = snap
        .oauth_providers
        .iter()
        .position(|row| row.entry_id == entry_id)?;
    let provider = &snap.oauth_providers[idx];
    let binding = snap
        .oauth_binding_hints
        .get(idx)
        .map(String::as_str)
        .unwrap_or("binding unknown");
    Some(if provider.enabled {
        format!("provider ready · {binding}")
    } else {
        format!("provider disabled · {binding}")
    })
}

fn current_auth_config_label(row: &McpConfigCatalogRow, snap: &UiSnapshot) -> String {
    let mut configs = Vec::new();
    if row.api_secret_present {
        configs.push("api key set".to_string());
    }
    if let Some(oauth) = oauth_provider_summary(snap, &row.entry_id) {
        configs.push(format!("oauth {oauth}"));
    }
    if configs.is_empty() && row.connect_profile.has_public_mode {
        "public".into()
    } else if configs.is_empty() {
        "unconfigured".into()
    } else {
        configs.join(" + ")
    }
}

/// Single-line catalogue list row clipped to pane width (full status in Details).
fn format_api_catalogue_row(
    selected: bool,
    on: bool,
    name: &str,
    auth_summary: &str,
    inner_cols: u16,
) -> Line<'static> {
    let mark = if on { "[on]" } else { "[off]" };
    let prefix = if selected { "› " } else { "  " };
    let plain = format!("{prefix}{mark} {name}  {auth_summary}");
    let clipped = log_render::clip_line_display(&plain, inner_cols.max(1));
    let row_style = if selected {
        selected_row_style()
    } else {
        Style::default()
    };
    let mark_style = if selected {
        selected_row_style()
    } else if on {
        api_toggle_on_style()
    } else {
        api_toggle_off_style()
    };
    Line::from(vec![Span::styled(
        clipped,
        if selected { mark_style } else { row_style },
    )])
}

/// Clip a list row built from parts (OAuth providers, keys, etc.).
fn clip_list_row_plain(parts: &str, inner_cols: u16) -> String {
    log_render::clip_line_display(parts, inner_cols.max(1))
}

/// Drain crossterm events until idle; resize updates terminal geometry.
fn drain_crossterm_events(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    timeout: Duration,
) -> Result<Vec<Event>, io::Error> {
    let mut out = Vec::new();
    if !event::poll(timeout)? {
        return Ok(out);
    }
    loop {
        match event::read()? {
            Event::Resize(w, h) => {
                terminal.resize(ratatui::layout::Rect::new(0, 0, w, h))?;
                out.push(Event::Resize(w, h));
            }
            other => out.push(other),
        }
        if !event::poll(Duration::from_millis(0))? {
            break;
        }
    }
    Ok(out)
}

fn api_secret_notice(entry_id: &str) -> RunNotice {
    RunNotice::new(
        NoticeSeverity::Success,
        "API key stored",
        format!("Stored the API secret for {entry_id}."),
    )
    .with_action_hint("Requests for this catalogue can now resolve the hosted secret locally.")
    .with_sticky(false)
}

fn apply_oauth_binding_to_snapshot(state: &mut RunState, entry_id: &str) {
    if let Some(ix) = state
        .resources
        .snapshot
        .oauth_providers
        .iter()
        .position(|row| row.entry_id == entry_id)
    {
        if let Some(hint) = state.resources.snapshot.oauth_binding_hints.get_mut(ix) {
            *hint = "binding updated — refreshing…".into();
        }
    }
    for row in &mut state.resources.snapshot.catalog_rows {
        if row.entry_id != entry_id {
            continue;
        }
        row.has_auth_binding = true;
        if matches!(row.auth_marker, McpCatalogAuthMarker::MissingBinding) {
            row.auth_marker = McpCatalogAuthMarker::RequiresConnect;
        }
    }
}

fn apply_api_secret_to_snapshot(state: &mut RunState, entry_id: &str) {
    for row in &mut state.resources.snapshot.catalog_rows {
        if row.entry_id == entry_id {
            row.api_secret_present = true;
        }
    }
}

fn select_oauth_config_from_api(state: &mut RunState, entry_id: &str) {
    state.screen = RunScreen::OAuth;
    if let Some(ix) = state
        .resources
        .snapshot
        .oauth_providers
        .iter()
        .position(|row| row.entry_id == entry_id)
    {
        state.oauth.selected = ix;
        set_notice(
            state,
            RunNotice::new(
                NoticeSeverity::Info,
                "OAuth selected",
                format!("Selected the OAuth provider for {entry_id}."),
            )
            .with_action_hint("Press d to bind/update the account, or x to disable the provider.")
            .with_sticky(false),
        );
    } else {
        state.mode = InputMode::OAuthWizard(OAuthUpsertWizard::for_entry(entry_id));
        set_notice(
            state,
            RunNotice::new(
                NoticeSeverity::Info,
                "Configure OAuth",
                format!("Create an OAuth provider for {entry_id} to use OAuth auth."),
            )
            .with_action_hint(
                "Complete the wizard, then run device authorization from the OAuth tab.",
            )
            .with_sticky(false),
        );
    }
}

fn device_bind_started_notice(
    entry_id: &str,
    prompt: &crate::appliance_oauth_admin::DeviceBindPrompt,
) -> RunNotice {
    let verification_target = prompt
        .verification_uri_complete
        .as_deref()
        .unwrap_or(prompt.verification_uri.as_str());
    RunNotice::new(
        NoticeSeverity::Info,
        "Bind started",
        format!("Open the verification URL for {entry_id} and enter the device code."),
    )
    .with_details(vec![
        format!("Open: {verification_target}"),
        format!("User code: {}", prompt.user_code),
        format!("Code lifetime: {}s", prompt.expires_in_secs),
        format!("Poll cadence: {}s", prompt.poll_interval_secs),
    ])
    .with_action_hint("Keep this screen open while the appliance waits for provider approval.")
    .with_sticky(true)
}

fn device_bind_success_notice(
    entry_id: &str,
    out: &crate::appliance_oauth_admin::DeviceBindOutcome,
) -> RunNotice {
    let verification_target = out
        .verification_uri_complete
        .as_deref()
        .unwrap_or(out.verification_uri.as_str());
    RunNotice::new(
        NoticeSeverity::Success,
        "Device bound",
        format!("OAuth token stored for {entry_id}."),
    )
    .with_details(vec![
        format!("Open: {verification_target}"),
        format!("User code: {}", out.user_code),
        format!("Expires in: {}s", out.expires_in_secs),
        format!("Poll cadence: {}s", out.poll_interval_secs),
    ])
    .with_action_hint("Use this provider normally; rerun d if you need to refresh the binding.")
}

fn device_bind_error_notice(entry_id: &str, raw_error: &str) -> RunNotice {
    let lowered = raw_error.to_ascii_lowercase();
    let (summary, hint) = if lowered.contains("device_flow_disabled") {
        (
            format!("{entry_id} rejected device authorization."),
            "Enable device flow for this OAuth app or use a different auth path.".to_string(),
        )
    } else if lowered.contains("device_authorization_endpoint missing") {
        (
            format!("{entry_id} is missing a device authorization endpoint."),
            "Upsert this provider with a device authorization URL before pressing d.".to_string(),
        )
    } else if lowered.contains("timed out") {
        (
            format!("{entry_id} device authorization timed out."),
            "Start the bind again when you are ready to approve it within the device-flow window."
                .to_string(),
        )
    } else if lowered.contains("oauth provider catalog entry missing")
        || lowered.contains("catalog unavailable")
    {
        (
            format!("{entry_id} is unavailable in the OAuth catalog."),
            "Restore or re-link the provider configuration, then try device bind again."
                .to_string(),
        )
    } else if lowered.contains("secret not available")
        || lowered.contains("client secret")
        || lowered.contains("bad_secret_utf8")
    {
        (
            format!("{entry_id} cannot start device authorization with its stored client secret."),
            "Repair the provider client secret in the appliance or CLI, then retry.".to_string(),
        )
    } else if lowered.contains("storage error") || lowered.contains("auth storage unavailable") {
        (
            format!("{entry_id} could not store OAuth state."),
            "Fix the appliance auth storage or local database state before retrying device bind."
                .to_string(),
        )
    } else {
        (
            format!("{entry_id} device authorization failed."),
            "Review the raw provider error below and adjust the provider configuration if needed."
                .to_string(),
        )
    };
    RunNotice::new(NoticeSeverity::Error, "Bind failed", summary)
        .with_details(vec![raw_error.to_string()])
        .with_action_hint(hint)
}

fn copy_notice(
    success_title: impl Into<String>,
    error_title: impl Into<String>,
    copy_result: Result<(), String>,
) -> RunNotice {
    match copy_result {
        Ok(()) => RunNotice::new(
            NoticeSeverity::Success,
            success_title,
            "Copied to the clipboard.",
        )
        .with_sticky(false),
        Err(e) => RunNotice::new(
            NoticeSeverity::Error,
            error_title,
            "Clipboard operation failed.",
        )
        .with_details(vec![e])
        .with_action_hint("Verify clipboard access for this terminal session and try again."),
    }
}

#[derive(Clone, Default)]
struct UiSnapshot {
    config_surface: McpConfigSurfaceState,
    catalog_rows: Vec<McpConfigCatalogRow>,
    keys: Vec<McpConfigApiKeyRow>,
    db_allowed: HashSet<String>,
    oauth_providers: Vec<plasm_agent_core::oauth_provider_repository::OauthProviderAppRow>,
    oauth_binding_hints: Vec<String>,
    oauth_surface: OAuthSurfaceState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RunScreen {
    Status,
    Clients,
    Apis,
    OAuth,
    Keys,
    Runs,
    Storage,
    Logs,
}

impl RunScreen {
    const ALL: [Self; 8] = [
        Self::Status,
        Self::Clients,
        Self::Apis,
        Self::OAuth,
        Self::Keys,
        Self::Runs,
        Self::Storage,
        Self::Logs,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Status => "Status",
            Self::Clients => "Clients",
            Self::Apis => "APIs",
            Self::OAuth => "OAuth",
            Self::Keys => "Keys",
            Self::Runs => "Runs",
            Self::Storage => "Storage",
            Self::Logs => "Logs",
        }
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn prev(self) -> Self {
        Self::ALL[self.index().checked_sub(1).unwrap_or(Self::ALL.len() - 1)]
    }

    fn index(self) -> usize {
        match self {
            Self::Status => 0,
            Self::Clients => 1,
            Self::Apis => 2,
            Self::OAuth => 3,
            Self::Keys => 4,
            Self::Runs => 5,
            Self::Storage => 6,
            Self::Logs => 7,
        }
    }
}

#[derive(Clone, Debug)]
enum InputMode {
    Normal,
    ApiFilter,
    ApiSecretEdit {
        entry_id: String,
        hosted_kv_key: String,
        buf: String,
    },
    CatalogConnect {
        entry_id: String,
        hosted_kv_key: String,
        step: u8,
        workspace_url: String,
        secret_buf: String,
    },
    AddKeyLabel {
        buf: String,
    },
    OAuthWizard(OAuthUpsertWizard),
    /// Choose CGS `oauth.scopes` before device authorization (avoids provider `default_scopes`).
    OAuthDeviceScopePick(OAuthDeviceScopePickState),
    ConfirmOAuthDisable {
        entry_id: String,
    },
    ConfirmKeyRevoke {
        key_id: Uuid,
    },
}

#[derive(Default)]
struct ApiState {
    selected: usize,
    /// Indices into `snapshot.catalog_rows` after filter.
    filtered_ix: Vec<usize>,
    filter: String,
    staged_allowed: Option<HashSet<String>>,
}

#[derive(Default)]
struct OAuthState {
    selected: usize,
}

#[derive(Default)]
struct KeysState {
    selected: usize,
}

#[derive(Default)]
struct LogState {
    lines: VecDeque<appliance_log::ApplianceLogEntry>,
    scroll: usize,
    /// Selected line index; viewport scroll is synced in [`render_running_frame`].
    cursor: usize,
}

#[derive(Default)]
struct OverviewState {
    scroll: u16,
}

#[derive(Default)]
struct ClientsState {
    scroll: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdminTaskKind {
    Refreshing,
    ProvisioningKey,
    SavingApiAllowlist,
    SavingApiSecret,
    DeviceAuthorization,
    SavingOAuthProvider,
    DisablingOAuthProvider,
    RotatingKey,
    RevokingKey,
    RevealingKey,
    CopyingMcpJson,
    CopyingPlasmCliProfile,
}

impl AdminTaskKind {
    fn label(self) -> &'static str {
        match self {
            Self::Refreshing => "Refreshing…",
            Self::ProvisioningKey => "Provisioning key…",
            Self::SavingApiAllowlist => "Saving API allowlist…",
            Self::SavingApiSecret => "Saving API secret…",
            Self::DeviceAuthorization => "Device authorization…",
            Self::SavingOAuthProvider => "Saving OAuth provider…",
            Self::DisablingOAuthProvider => "Disabling OAuth provider…",
            Self::RotatingKey => "Rotating key…",
            Self::RevokingKey => "Revoking key…",
            Self::RevealingKey => "Revealing key…",
            Self::CopyingMcpJson => "Copying MCP config…",
            Self::CopyingPlasmCliProfile => "Copying plasm CLI profile…",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingAdminTask {
    corr: AdminCorr,
    kind: AdminTaskKind,
    started_at: Instant,
}

#[derive(Default)]
struct AdminSyncState {
    /// Monotonic correlation id for async admin jobs.
    next_corr: AdminCorr,
    refresh: Option<PendingAdminTask>,
    inline: Option<PendingAdminTask>,
}

impl AdminSyncState {
    fn pending_refresh_corr(&self) -> Option<AdminCorr> {
        self.refresh.map(|task| task.corr)
    }

    fn pending_inline_corr(&self) -> Option<AdminCorr> {
        self.inline.map(|task| task.corr)
    }

    fn start_refresh(&mut self, corr: AdminCorr) {
        self.refresh = Some(PendingAdminTask {
            corr,
            kind: AdminTaskKind::Refreshing,
            started_at: Instant::now(),
        });
    }

    fn start_inline(&mut self, corr: AdminCorr, kind: AdminTaskKind) {
        self.inline = Some(PendingAdminTask {
            corr,
            kind,
            started_at: Instant::now(),
        });
    }

    fn finish_refresh(&mut self, corr: AdminCorr) -> bool {
        if self.pending_refresh_corr() == Some(corr) {
            self.refresh = None;
            return true;
        }
        false
    }

    fn finish_inline(&mut self, corr: AdminCorr) -> Option<AdminTaskKind> {
        if self.pending_inline_corr() == Some(corr) {
            return self.inline.take().map(|task| task.kind);
        }
        None
    }

    fn busy_task(&self) -> Option<PendingAdminTask> {
        self.inline.or(self.refresh)
    }
}

#[derive(Default)]
struct ResourceState {
    snapshot: UiSnapshot,
    config_id: Option<Uuid>,
    admin: AdminSyncState,
}

struct RunState {
    screen: RunScreen,
    mode: InputMode,
    api: ApiState,
    oauth: OAuthState,
    keys: KeysState,
    logs: LogState,
    resources: ResourceState,
    notice: Option<RunNotice>,
    overview: OverviewState,
    clients: ClientsState,
    policy_bootstrap_detail: Option<PolicyStoreBootstrapDetail>,
}

impl RunState {
    fn new() -> Self {
        Self {
            screen: RunScreen::Status,
            mode: InputMode::Normal,
            api: ApiState::default(),
            oauth: OAuthState::default(),
            keys: KeysState::default(),
            logs: LogState::default(),
            overview: OverviewState::default(),
            clients: ClientsState::default(),
            resources: ResourceState::default(),
            notice: None,
            policy_bootstrap_detail: None,
        }
    }

    fn recompute_filter(&mut self, rows: &[McpConfigCatalogRow]) {
        let f = self.api.filter.trim().to_ascii_lowercase();
        self.api.filtered_ix.clear();
        for (i, r) in rows.iter().enumerate() {
            if f.is_empty()
                || r.entry_id.to_ascii_lowercase().contains(&f)
                || r.label.to_ascii_lowercase().contains(&f)
            {
                self.api.filtered_ix.push(i);
            }
        }
        if self.api.selected >= self.api.filtered_ix.len() {
            self.api.selected = self.api.filtered_ix.len().saturating_sub(1);
        }
    }

    fn add_key_label_buf(&self) -> Option<&str> {
        match &self.mode {
            InputMode::AddKeyLabel { buf } => Some(buf.as_str()),
            _ => None,
        }
    }

    fn pending_oauth_disable_entry(&self) -> Option<&str> {
        match &self.mode {
            InputMode::ConfirmOAuthDisable { entry_id } => Some(entry_id.as_str()),
            _ => None,
        }
    }

    fn admin_busy(&self) -> bool {
        self.resources.admin.inline.is_some()
    }

    fn reset_screen_local_mode(&mut self) {
        let reset = matches!(
            (&self.screen, &self.mode),
            (RunScreen::Apis, InputMode::ApiFilter)
                | (RunScreen::Apis, InputMode::ApiSecretEdit { .. })
                | (RunScreen::Apis, InputMode::CatalogConnect { .. })
                | (RunScreen::OAuth, InputMode::OAuthWizard(_))
                | (RunScreen::OAuth, InputMode::OAuthDeviceScopePick(_))
                | (RunScreen::OAuth, InputMode::ConfirmOAuthDisable { .. })
                | (RunScreen::Keys, InputMode::AddKeyLabel { .. })
                | (RunScreen::Keys, InputMode::ConfirmKeyRevoke { .. })
        );
        if !reset && !matches!(self.mode, InputMode::Normal) {
            self.mode = InputMode::Normal;
        }
    }
}

fn alloc_admin_corr(state: &mut RunState) -> AdminCorr {
    state.resources.admin.next_corr = state.resources.admin.next_corr.wrapping_add(1).max(1);
    state.resources.admin.next_corr
}

fn enqueue_refresh_if_idle(state: &mut RunState, bridge: &AdminBridge) {
    if state.resources.admin.refresh.is_some() {
        return;
    }
    enqueue_refresh_force(state, bridge);
}

/// Queue a full snapshot refresh and supersede any in-flight refresh correlation (stale completions ignored).
fn enqueue_refresh_force(state: &mut RunState, bridge: &AdminBridge) {
    let c = alloc_admin_corr(state);
    state.resources.admin.start_refresh(c);
    if bridge
        .jobs_tx
        .send(AdminJob::RefreshFull { corr: c })
        .is_err()
    {
        state.resources.admin.refresh = None;
        set_notice(
            state,
            RunNotice::new(
                NoticeSeverity::Error,
                "Admin bridge unavailable",
                "Admin router queue closed — restart the appliance.",
            ),
        );
    }
}

fn submit_inline_admin_job(
    state: &mut RunState,
    bridge: &AdminBridge,
    kind: AdminTaskKind,
    build: impl FnOnce(AdminCorr) -> AdminJob,
) {
    let c = alloc_admin_corr(state);
    state.resources.admin.start_inline(c, kind);
    let job = build(c);
    if bridge.jobs_tx.send(job).is_err() {
        state.resources.admin.inline = None;
        set_notice(
            state,
            RunNotice::new(
                NoticeSeverity::Error,
                "Admin bridge unavailable",
                "Admin router queue closed — restart the appliance.",
            ),
        );
    }
}

fn apply_refreshed_ui_data(state: &mut RunState, data: RefreshedUiData) {
    state.resources.config_id = data.config_id;
    state.resources.snapshot.config_surface = data.config_surface;
    state.resources.snapshot.catalog_rows = data.catalog_rows;
    state.resources.snapshot.keys = data.keys;
    state.resources.snapshot.db_allowed = data.db_allowed;
    state.resources.snapshot.oauth_providers = data.oauth_providers;
    state.resources.snapshot.oauth_binding_hints = data.oauth_binding_hints;
    state.resources.snapshot.oauth_surface = data.oauth_surface;
}

fn apply_admin_completion(
    state: &mut RunState,
    bridge: Option<&AdminBridge>,
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    comp: AdminCompletion,
) {
    match comp {
        AdminCompletion::RefreshFull { corr, data } => {
            if state.resources.admin.finish_refresh(corr) {
                apply_refreshed_ui_data(state, data);
                let rows = state.resources.snapshot.catalog_rows.clone();
                state.recompute_filter(&rows);
                if state.oauth.selected >= state.resources.snapshot.oauth_providers.len() {
                    state.oauth.selected = state
                        .resources
                        .snapshot
                        .oauth_providers
                        .len()
                        .saturating_sub(1);
                }
            }
        }
        AdminCompletion::ProvisionApiKey { corr, result } => {
            if state.resources.admin.finish_inline(corr).is_some() {
                match result {
                    Ok(_) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Success,
                            "API key provisioned",
                            "Created a new transport API key.",
                        )
                        .with_sticky(false),
                    ),
                    Err(e) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Error,
                            "API key provision failed",
                            "Could not create a new transport API key.",
                        )
                        .with_details(vec![e]),
                    ),
                }
                if let Some(bridge) = bridge {
                    enqueue_refresh_force(state, bridge);
                }
            }
        }
        AdminCompletion::SetAllowedApisExact { corr, result } => {
            if state.resources.admin.finish_inline(corr).is_some() {
                match result {
                    Ok(()) => {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Success,
                                "API allowlist saved",
                                "Saved the current API selection for this appliance.",
                            )
                            .with_sticky(false),
                        );
                        state.api.staged_allowed = None;
                    }
                    Err(e) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Error,
                            "API allowlist save failed",
                            "Could not save the selected APIs.",
                        )
                        .with_details(vec![e]),
                    ),
                }
                if let Some(bridge) = bridge {
                    enqueue_refresh_force(state, bridge);
                }
            }
        }
        AdminCompletion::StoreOutboundSecret { corr, key, result } => {
            if state.resources.admin.finish_inline(corr).is_some() {
                let entry_id = state
                    .resources
                    .snapshot
                    .catalog_rows
                    .iter()
                    .find(|row| row.api_secret_hosted_kv.as_deref() == Some(key.as_str()))
                    .map(|row| row.entry_id.clone());
                match result {
                    Ok(()) => {
                        if let Some(ref entry_id) = entry_id {
                            apply_api_secret_to_snapshot(state, entry_id);
                            set_notice(state, api_secret_notice(entry_id));
                        } else {
                            set_notice(
                                state,
                                RunNotice::new(
                                    NoticeSeverity::Success,
                                    "API key stored",
                                    "Stored the API key secret.",
                                )
                                .with_sticky(false),
                            );
                        }
                    }
                    Err(e) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Error,
                            "API key store failed",
                            "Could not store the API key secret.",
                        )
                        .with_details(vec![e]),
                    ),
                }
                if let Some(bridge) = bridge {
                    enqueue_refresh_force(state, bridge);
                }
            }
        }
        AdminCompletion::StoreMcpCatalogBinding {
            corr,
            entry_id,
            result,
        } => {
            if state.resources.admin.finish_inline(corr).is_some() {
                match result {
                    Ok(()) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Success,
                            "Bindings stored",
                            format!("Saved workspace binding for {entry_id}."),
                        )
                        .with_sticky(false),
                    ),
                    Err(e) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Error,
                            "Binding store failed",
                            format!("Could not store bindings for {entry_id}."),
                        )
                        .with_details(vec![e]),
                    ),
                }
                if let Some(bridge) = bridge {
                    enqueue_refresh_force(state, bridge);
                }
            }
        }
        AdminCompletion::OAuthDeviceBindStarted { corr, prompt } => {
            if state.resources.admin.pending_inline_corr() == Some(corr) {
                let entry_id = selected_oauth_entry_id(state)
                    .unwrap_or("selected provider")
                    .to_string();
                set_notice(state, device_bind_started_notice(&entry_id, &prompt));
            }
        }
        AdminCompletion::OAuthDeviceBind { corr, result } => {
            if state.resources.admin.finish_inline(corr).is_some() {
                match result {
                    Ok(out) => {
                        let entry_id = selected_oauth_entry_id(state)
                            .unwrap_or("selected provider")
                            .to_string();
                        apply_oauth_binding_to_snapshot(state, &entry_id);
                        set_notice(state, device_bind_success_notice(&entry_id, &out));
                    }
                    Err(e) => {
                        let entry_id = selected_oauth_entry_id(state)
                            .unwrap_or("selected provider")
                            .to_string();
                        set_notice(state, device_bind_error_notice(&entry_id, &e));
                    }
                }
                if let Some(bridge) = bridge {
                    enqueue_refresh_force(state, bridge);
                }
            }
        }
        AdminCompletion::OauthProviderUpsert { corr, result } => {
            if state.resources.admin.finish_inline(corr).is_some() {
                match result {
                    Ok(()) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Success,
                            "OAuth provider saved",
                            "Saved the provider configuration.",
                        )
                        .with_sticky(false),
                    ),
                    Err(e) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Error,
                            "OAuth provider save failed",
                            "Could not save the provider configuration.",
                        )
                        .with_details(vec![e]),
                    ),
                }
                if let Some(bridge) = bridge {
                    enqueue_refresh_force(state, bridge);
                }
            }
        }
        AdminCompletion::OauthProviderDisable { corr, result } => {
            if state.resources.admin.finish_inline(corr).is_some() {
                match result {
                    Ok(()) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Success,
                            "OAuth provider disabled",
                            "Disabled the selected provider.",
                        )
                        .with_sticky(false),
                    ),
                    Err(e) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Error,
                            "OAuth disable failed",
                            "Could not disable the selected provider.",
                        )
                        .with_details(vec![e]),
                    ),
                }
                if let Some(bridge) = bridge {
                    enqueue_refresh_force(state, bridge);
                }
            }
        }
        AdminCompletion::RotateApiKey { corr, result } => {
            if state.resources.admin.finish_inline(corr).is_some() {
                match result {
                    Ok(_) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Success,
                            "API key rotated",
                            "Replaced the selected transport API key.",
                        )
                        .with_sticky(false),
                    ),
                    Err(e) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Error,
                            "API key rotate failed",
                            "Could not rotate the selected transport API key.",
                        )
                        .with_details(vec![e]),
                    ),
                }
                if let Some(bridge) = bridge {
                    enqueue_refresh_force(state, bridge);
                }
            }
        }
        AdminCompletion::RevokeApiKey { corr, result } => {
            if state.resources.admin.finish_inline(corr).is_some() {
                match result {
                    Ok(()) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Success,
                            "API key revoked",
                            "Removed the selected transport API key.",
                        )
                        .with_sticky(false),
                    ),
                    Err(e) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Error,
                            "API key revoke failed",
                            "Could not revoke the selected transport API key.",
                        )
                        .with_details(vec![e]),
                    ),
                }
                if let Some(bridge) = bridge {
                    enqueue_refresh_force(state, bridge);
                }
            }
        }
        AdminCompletion::RevealApiKey { corr, result } => {
            if let Some(kind) = state.resources.admin.finish_inline(corr) {
                match (kind, result) {
                    (AdminTaskKind::RevealingKey, Ok(raw)) => set_notice(
                        state,
                        copy_notice(
                            "API key secret copied",
                            "API key secret copy failed",
                            copy_text_to_clipboard(&raw),
                        ),
                    ),
                    (AdminTaskKind::CopyingMcpJson, Ok(raw)) => {
                        match mcp_client_json_config(listen, Some(&raw)) {
                            Ok(json) => set_notice(
                                state,
                                copy_notice(
                                    "MCP client config copied",
                                    "MCP client config copy failed",
                                    copy_text_to_clipboard(&json),
                                ),
                            ),
                            Err(e) => set_notice(
                                state,
                                RunNotice::new(
                                    NoticeSeverity::Error,
                                    "MCP client config build failed",
                                    "Could not build MCP JSON for clipboard.",
                                )
                                .with_details(vec![e]),
                            ),
                        }
                    }
                    (AdminTaskKind::CopyingPlasmCliProfile, Ok(raw)) => {
                        match plasm_cli_profile_json_config(listen, Some(&raw)) {
                            Ok(json) => set_notice(
                                state,
                                copy_notice(
                                    "Plasm CLI profile copied",
                                    "Plasm CLI profile copy failed",
                                    copy_text_to_clipboard(&json),
                                ),
                            ),
                            Err(e) => set_notice(
                                state,
                                RunNotice::new(
                                    NoticeSeverity::Error,
                                    "Plasm CLI profile build failed",
                                    "Could not build ~/.plasm/cgs/profiles JSON for clipboard.",
                                )
                                .with_details(vec![e]),
                            ),
                        }
                    }
                    (_, Err(e)) => set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Error,
                            "API key reveal failed",
                            "Could not reveal the selected API key secret.",
                        )
                        .with_details(vec![e]),
                    ),
                    _ => {}
                }
            }
        }
    }
}

enum UiMsg {
    Tick,
    Key(KeyEvent),
    Admin(Box<AdminCompletion>),
    LogLine(appliance_log::ApplianceLogEntry),
}

fn row_enabled(state: &RunState, snap: &UiSnapshot, entry_id: &str) -> bool {
    if let Some(ref staged) = state.api.staged_allowed {
        return staged.contains(entry_id);
    }
    snap.db_allowed.contains(entry_id)
}

/// `(entry_id, missing)` pairs for enabled MCP allowlist rows (secret and/or binding slots).
fn catalog_policy_readiness_gaps(
    rows: &[McpConfigCatalogRow],
    allowed: &std::collections::HashSet<String>,
) -> Vec<(String, String)> {
    rows.iter()
        .filter(|r| allowed.contains(&r.entry_id))
        .flat_map(|r| {
            let mut gaps = Vec::new();
            if r.connect_profile.has_api_key && !r.auth_optional && !r.api_secret_present {
                gaps.push((r.entry_id.clone(), "secret".to_string()));
            }
            if r.bindings_required && !r.bindings_complete {
                gaps.push((r.entry_id.clone(), "binding".to_string()));
            }
            gaps
        })
        .collect()
}

fn oauth_surface_status(snap: &UiSnapshot) -> Option<&str> {
    snap.oauth_surface.status_message()
}

fn input_mode_label(mode: &InputMode) -> Option<&'static str> {
    match mode {
        InputMode::Normal => None,
        InputMode::ApiFilter => Some("API filter"),
        InputMode::ApiSecretEdit { .. } => Some("API key secret"),
        InputMode::CatalogConnect { .. } => Some("Catalog connect"),
        InputMode::AddKeyLabel { .. } => Some("Add key"),
        InputMode::ConfirmKeyRevoke { .. } => Some("Confirm revoke"),
        InputMode::OAuthWizard(_) => Some("OAuth wizard"),
        InputMode::OAuthDeviceScopePick(_) => Some("OAuth scopes"),
        InputMode::ConfirmOAuthDisable { .. } => Some("Confirm disable"),
    }
}

struct UpdateDeps<'a> {
    admin_bridge: Option<&'a AdminBridge>,
    host_state: Option<&'a PlasmHostState>,
    listen: &'a plasm_agent_core::listen_endpoint::TcpListenEndpoint,
}

fn update_modal_key(state: &mut RunState, key: KeyEvent, deps: &UpdateDeps<'_>) -> bool {
    let admin_busy = state.admin_busy();
    match &mut state.mode {
        InputMode::ApiFilter => match key.code {
            KeyCode::Enter | KeyCode::Tab | KeyCode::BackTab => state.mode = InputMode::Normal,
            KeyCode::Esc => {
                state.mode = InputMode::Normal;
                state.api.filter.clear();
                let rows = state.resources.snapshot.catalog_rows.clone();
                state.recompute_filter(&rows);
            }
            KeyCode::Backspace => {
                state.api.filter.pop();
                let rows = state.resources.snapshot.catalog_rows.clone();
                state.recompute_filter(&rows);
            }
            KeyCode::Char(c) => {
                state.api.filter.push(c);
                let rows = state.resources.snapshot.catalog_rows.clone();
                state.recompute_filter(&rows);
            }
            _ => {}
        },
        InputMode::ApiSecretEdit {
            entry_id: _,
            hosted_kv_key,
            buf,
        } => match key.code {
            KeyCode::Enter => {
                let secret = buf.trim().to_string();
                let key = hosted_kv_key.clone();
                state.mode = InputMode::Normal;
                if !secret.is_empty() {
                    if state.admin_busy() {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Warning,
                                "Busy",
                                "Wait for the current admin task to finish.",
                            )
                            .with_sticky(false),
                        );
                    } else if let Some(bridge) = deps.admin_bridge {
                        submit_inline_admin_job(
                            state,
                            bridge,
                            AdminTaskKind::SavingApiSecret,
                            |c| AdminJob::StoreOutboundSecret {
                                corr: c,
                                key,
                                value: secret,
                            },
                        );
                    } else {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Error,
                                "Auth storage unavailable",
                                "Cannot save the API key without the admin bridge.",
                            ),
                        );
                    }
                }
            }
            KeyCode::Esc => state.mode = InputMode::Normal,
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) => buf.push(c),
            _ => {}
        },
        InputMode::CatalogConnect {
            entry_id,
            hosted_kv_key,
            step,
            workspace_url,
            secret_buf,
        } => match key.code {
            KeyCode::Enter => {
                if *step == 0 {
                    let url = workspace_url.trim().to_string();
                    if url.is_empty() || !url.starts_with("http") {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Warning,
                                "Invalid URL",
                                "Workspace URL must start with http:// or https://",
                            )
                            .with_sticky(false),
                        );
                    } else {
                        *step = 1;
                        let entry_label = entry_id.clone();
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Info,
                                "Connect catalog",
                                format!("Step 2/2: API key for {entry_label}."),
                            )
                            .with_action_hint("Enter save · Esc cancel")
                            .with_sticky(false),
                        );
                    }
                } else {
                    let secret = secret_buf.trim().to_string();
                    let url = workspace_url.trim().trim_end_matches('/').to_string();
                    let entry = entry_id.clone();
                    let kv_key = hosted_kv_key.clone();
                    state.mode = InputMode::Normal;
                    if secret.is_empty() || url.is_empty() {
                        return false;
                    }
                    let token = if secret.starts_with("Token ") {
                        secret
                    } else {
                        format!("Token {secret}")
                    };
                    let payload = serde_json::json!({
                        "version": 1,
                        "entry_id": entry,
                        "access_token": token,
                    });
                    if let (Some(bridge), Some(cid)) =
                        (deps.admin_bridge, state.resources.config_id)
                    {
                        let tenant = appliance_mcp_scope().tenant_id;
                        let values = std::collections::HashMap::from([(
                            "catalog_http_origin".to_string(),
                            url,
                        )]);
                        submit_inline_admin_job(
                            state,
                            bridge,
                            AdminTaskKind::SavingApiSecret,
                            |c| AdminJob::StoreOutboundSecret {
                                corr: c,
                                key: kv_key,
                                value: payload.to_string(),
                            },
                        );
                        submit_inline_admin_job(
                            state,
                            bridge,
                            AdminTaskKind::SavingApiSecret,
                            move |c| AdminJob::StoreMcpCatalogBinding {
                                corr: c,
                                tenant_id: tenant,
                                config_id: cid,
                                entry_id: entry,
                                values,
                            },
                        );
                    }
                }
            }
            KeyCode::Esc => state.mode = InputMode::Normal,
            KeyCode::Backspace => {
                if *step == 0 {
                    workspace_url.pop();
                } else {
                    secret_buf.pop();
                }
            }
            KeyCode::Char(c) => {
                if *step == 0 {
                    workspace_url.push(c);
                } else {
                    secret_buf.push(c);
                }
            }
            _ => {}
        },
        InputMode::AddKeyLabel { buf } => match key.code {
            KeyCode::Enter => {
                let label = buf.trim().to_string();
                state.mode = InputMode::Normal;
                if !label.is_empty() {
                    if state.admin_busy() {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Warning,
                                "Busy",
                                "Wait for the current admin task to finish.",
                            )
                            .with_sticky(false),
                        );
                    } else if let (Some(bridge), Some(cid)) =
                        (deps.admin_bridge, state.resources.config_id)
                    {
                        submit_inline_admin_job(
                            state,
                            bridge,
                            AdminTaskKind::ProvisioningKey,
                            |c| AdminJob::ProvisionApiKey {
                                corr: c,
                                config_id: cid,
                                label,
                            },
                        );
                    } else if deps.admin_bridge.is_some() && state.resources.config_id.is_none() {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Warning,
                                "Config still loading",
                                "Wait for the appliance config refresh before provisioning a key.",
                            )
                            .with_sticky(false),
                        );
                    }
                }
            }
            KeyCode::Esc => state.mode = InputMode::Normal,
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) => buf.push(c),
            _ => {}
        },
        InputMode::OAuthWizard(wiz) => {
            let rows = &state.resources.snapshot.catalog_rows;
            match key.code {
                KeyCode::Esc => {
                    state.mode = InputMode::Normal;
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Info,
                            "OAuth wizard cancelled",
                            "Dismissed the provider upsert wizard.",
                        )
                        .with_sticky(false),
                    );
                }
                KeyCode::Enter => {
                    if wiz.step == OAuthUpsertStep::Confirm {
                        match wiz.try_build_upsert() {
                            Ok(upsert) => {
                                if state.admin_busy() {
                                    set_notice(
                                        state,
                                        RunNotice::new(
                                            NoticeSeverity::Warning,
                                            "Busy",
                                            "Wait for the current admin task to finish.",
                                        )
                                        .with_sticky(false),
                                    );
                                } else if let Some(bridge) = deps.admin_bridge {
                                    state.mode = InputMode::Normal;
                                    submit_inline_admin_job(
                                        state,
                                        bridge,
                                        AdminTaskKind::SavingOAuthProvider,
                                        |c| AdminJob::OauthProviderUpsert { corr: c, upsert },
                                    );
                                } else {
                                    set_notice(
                                        state,
                                        RunNotice::new(
                                            NoticeSeverity::Error,
                                            "Admin bridge unavailable",
                                            "Cannot save the provider without the admin bridge.",
                                        ),
                                    );
                                }
                            }
                            Err(e) => set_notice(
                                state,
                                RunNotice::new(
                                    NoticeSeverity::Error,
                                    "OAuth provider review failed",
                                    "The provider settings are incomplete or invalid.",
                                )
                                .with_details(vec![e]),
                            ),
                        }
                    } else if wiz.step == OAuthUpsertStep::Enabled {
                        wiz.advance_enabled_to_confirm();
                    } else if wiz.step == OAuthUpsertStep::EntryId {
                        if let Err(msg) = wiz.commit_entry_selection(rows) {
                            set_notice(
                                state,
                                RunNotice::new(
                                    NoticeSeverity::Warning,
                                    "Choose a provider",
                                    "Select a registry API before continuing.",
                                )
                                .with_details(vec![msg.to_string()])
                                .with_sticky(false),
                            );
                        }
                    } else if let Err(msg) = wiz.commit_buf_and_advance() {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Warning,
                                "Field validation",
                                "Complete the current OAuth provider field before continuing.",
                            )
                            .with_details(vec![msg.to_string()])
                            .with_sticky(false),
                        );
                    }
                }
                KeyCode::Down | KeyCode::Char('j') if wiz.step == OAuthUpsertStep::EntryId => {
                    wiz.move_entry_selection(rows, 1);
                }
                KeyCode::Up | KeyCode::Char('k') if wiz.step == OAuthUpsertStep::EntryId => {
                    wiz.move_entry_selection(rows, -1);
                }
                KeyCode::Char(' ') if wiz.step == OAuthUpsertStep::Enabled => {
                    wiz.enabled = !wiz.enabled;
                }
                KeyCode::Backspace
                    if !matches!(
                        wiz.step,
                        OAuthUpsertStep::Enabled | OAuthUpsertStep::Confirm
                    ) =>
                {
                    wiz.buf.pop();
                    if wiz.step == OAuthUpsertStep::EntryId {
                        wiz.reset_entry_selection();
                    }
                }
                KeyCode::Char(c)
                    if !matches!(
                        wiz.step,
                        OAuthUpsertStep::Enabled | OAuthUpsertStep::Confirm
                    ) =>
                {
                    wiz.buf.push(c);
                    if wiz.step == OAuthUpsertStep::EntryId {
                        wiz.reset_entry_selection();
                    }
                }
                _ => {}
            }
        }
        InputMode::OAuthDeviceScopePick(ref mut pick) => match key.code {
            KeyCode::Esc => {
                state.mode = InputMode::Normal;
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Info,
                        "Device bind cancelled",
                        "Dismissed catalogue OAuth scope selection.",
                    )
                    .with_sticky(false),
                );
            }
            KeyCode::Enter => {
                let scopes = pick.selected_scope_strings();
                if scopes.is_empty() {
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Warning,
                            "No scopes selected",
                            "Choose at least one scope from the CGS catalogue (Space toggles the highlighted row).",
                        )
                        .with_sticky(false),
                    );
                } else if admin_busy {
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Warning,
                            "Busy",
                            "Wait for the current admin task to finish.",
                        )
                        .with_sticky(false),
                    );
                } else if let Some(bridge) = deps.admin_bridge {
                    let entry_id = pick.entry_id.clone();
                    let catalog = Arc::clone(&pick.link_catalog);
                    let storage = Arc::clone(&pick.storage);
                    state.mode = InputMode::Normal;
                    submit_inline_admin_job(
                        state,
                        bridge,
                        AdminTaskKind::DeviceAuthorization,
                        |c| AdminJob::OAuthDeviceBind {
                            corr: c,
                            entry_id,
                            scopes,
                            catalog,
                            storage,
                        },
                    );
                } else {
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Error,
                            "Admin bridge unavailable",
                            "Cannot start device authorization without the admin bridge.",
                        ),
                    );
                }
            }
            KeyCode::Down | KeyCode::Char('j') => pick.move_cursor(1),
            KeyCode::Up | KeyCode::Char('k') => pick.move_cursor(-1),
            KeyCode::Char(' ') => pick.toggle_cursor_row(),
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                let idx = (c as u8 - b'1') as usize;
                if let Some(name) = pick.apply_default_set(idx) {
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Info,
                            "Scope bundle applied",
                            format!("Loaded CGS default scope set `{name}`."),
                        )
                        .with_sticky(false),
                    );
                }
            }
            _ => {}
        },
        InputMode::Normal
        | InputMode::ConfirmOAuthDisable { .. }
        | InputMode::ConfirmKeyRevoke { .. } => {}
    }
    false
}

fn update_normal_key(state: &mut RunState, key: KeyEvent, deps: &UpdateDeps<'_>) -> bool {
    let snap = state.resources.snapshot.clone();
    match key.code {
        KeyCode::Char('q') => return true,
        KeyCode::Char('#') if state.screen == RunScreen::Clients => {
            let url = deps.listen.client_mcp_streamable_url();
            set_notice(
                state,
                copy_notice(
                    "MCP URL copied",
                    "MCP URL copy failed",
                    copy_text_to_clipboard(&url),
                ),
            );
        }
        KeyCode::Char('#') if state.screen == RunScreen::Keys => {
            if let Some(k) = snap.keys.get(state.keys.selected) {
                let line = api_key_row_copy_line(k);
                set_notice(
                    state,
                    copy_notice(
                        "Key label copied",
                        "Key label copy failed",
                        copy_text_to_clipboard(&line),
                    ),
                );
            } else {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "No key selected",
                        "There is no key row to copy.",
                    )
                    .with_sticky(false),
                );
            }
        }
        KeyCode::Right | KeyCode::Tab => {
            set_run_screen(state, state.screen.next());
            state.reset_screen_local_mode();
        }
        KeyCode::Left | KeyCode::BackTab => {
            set_run_screen(state, state.screen.prev());
            state.reset_screen_local_mode();
        }
        KeyCode::Esc
            if state.screen == RunScreen::OAuth
                && matches!(state.mode, InputMode::ConfirmOAuthDisable { .. }) =>
        {
            state.mode = InputMode::Normal;
            set_notice(
                state,
                RunNotice::new(
                    NoticeSeverity::Info,
                    "Disable cancelled",
                    "Dismissed the provider disable confirmation.",
                )
                .with_sticky(false),
            );
        }
        KeyCode::Esc
            if state.screen == RunScreen::Keys
                && matches!(state.mode, InputMode::ConfirmKeyRevoke { .. }) =>
        {
            state.mode = InputMode::Normal;
            set_notice(
                state,
                RunNotice::new(
                    NoticeSeverity::Info,
                    "Revoke cancelled",
                    "Dismissed the API key revoke confirmation.",
                )
                .with_sticky(false),
            );
        }
        KeyCode::Esc if dismiss_transient_notice(state) => {}
        KeyCode::Char('/') if state.screen == RunScreen::Apis => {
            state.mode = InputMode::ApiFilter;
        }
        KeyCode::Down | KeyCode::Char('j') if state.screen == RunScreen::Apis => {
            if state.api.selected + 1 < state.api.filtered_ix.len() {
                state.api.selected += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') if state.screen == RunScreen::Apis => {
            state.api.selected = state.api.selected.saturating_sub(1);
        }
        KeyCode::Char(' ') if state.screen == RunScreen::Apis => {
            if let Some(&rix) = state.api.filtered_ix.get(state.api.selected) {
                if let Some(r) = snap.catalog_rows.get(rix) {
                    let eid = r.entry_id.clone();
                    if state.api.staged_allowed.is_none() {
                        state.api.staged_allowed = Some(snap.db_allowed.clone());
                    }
                    if let Some(ref mut set) = state.api.staged_allowed {
                        if set.contains(&eid) {
                            set.remove(&eid);
                        } else {
                            set.insert(eid);
                        }
                    }
                }
            }
        }
        KeyCode::Char('s') if state.screen == RunScreen::Apis => {
            if state.admin_busy() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Busy",
                        "Wait for the current admin task to finish.",
                    )
                    .with_sticky(false),
                );
            } else if let (Some(bridge), Some(cid)) = (deps.admin_bridge, state.resources.config_id)
            {
                let set = state
                    .api
                    .staged_allowed
                    .clone()
                    .unwrap_or_else(|| snap.db_allowed.clone());
                let gaps = catalog_policy_readiness_gaps(&snap.catalog_rows, &set);
                if !gaps.is_empty() {
                    let detail = gaps
                        .iter()
                        .map(|(eid, slot)| format!("{eid} ({slot})"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Warning,
                            "MCP policy not ready",
                            format!(
                                "Complete connect for: {detail}. Press 'a' on each catalog row."
                            ),
                        )
                        .with_sticky(false),
                    );
                } else {
                    submit_inline_admin_job(
                        state,
                        bridge,
                        AdminTaskKind::SavingApiAllowlist,
                        |c| AdminJob::SetAllowedApisExact {
                            corr: c,
                            config_id: cid,
                            entry_ids: set,
                        },
                    );
                }
            }
        }
        KeyCode::Char('a') if state.screen == RunScreen::Apis => {
            if let Some(row) = selected_api_row(state, &snap) {
                let entry_id = row.entry_id.clone();
                let supports_api_key = row.connect_profile.has_api_key;
                let hosted_kv_key = row.api_secret_hosted_kv.clone();
                if !supports_api_key {
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Warning,
                            "API key unsupported",
                            format!("{entry_id} does not advertise API-key auth."),
                        )
                        .with_sticky(false),
                    );
                } else if let Some(hosted_kv_key) = hosted_kv_key {
                    if row.bindings_required && !row.bindings_complete {
                        state.mode = InputMode::CatalogConnect {
                            entry_id: entry_id.clone(),
                            hosted_kv_key,
                            step: 0,
                            workspace_url: String::new(),
                            secret_buf: String::new(),
                        };
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Info,
                                "Connect catalog",
                                format!("Step 1/2: workspace URL for {entry_id}."),
                            )
                            .with_action_hint("Enter next · Esc cancel")
                            .with_sticky(false),
                        );
                    } else {
                        state.mode = InputMode::ApiSecretEdit {
                            entry_id: entry_id.clone(),
                            hosted_kv_key,
                            buf: String::new(),
                        };
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Info,
                                "Set API key",
                                format!("Store an API key secret for {entry_id}."),
                            )
                            .with_action_hint(
                                "Type the secret, then press Enter to save it in local hosted KV.",
                            )
                            .with_sticky(false),
                        );
                    }
                } else {
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Warning,
                            "No hosted API secret slot",
                            format!(
                                "{entry_id} declares API auth via `env:` only (no `hosted_kv:` in domain.yaml). \
The control station stores secrets in auth-framework KV, so there is nowhere to write a key until the catalogue adds a `hosted_kv` path (you can keep `env` for shells — runtime uses KV when set, else env)."
                            ),
                        )
                        .with_action_hint(
                            "Add `hosted_kv: plasm:outbound:v1:…` next to `env:` under `auth:` for this catalogue, reload plugins, then press a again.",
                        )
                        .with_sticky(false),
                    );
                }
            }
        }
        KeyCode::Char('o') if state.screen == RunScreen::Apis => {
            if let Some(row) = selected_api_row(state, &snap) {
                if row.connect_profile.has_oauth {
                    let entry_id = row.entry_id.clone();
                    select_oauth_config_from_api(state, &entry_id);
                } else {
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Warning,
                            "OAuth unsupported",
                            format!("{} does not advertise OAuth auth.", row.entry_id),
                        )
                        .with_sticky(false),
                    );
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j')
            if state.screen == RunScreen::OAuth && matches!(state.mode, InputMode::Normal) =>
        {
            if state.oauth.selected + 1 < snap.oauth_providers.len() {
                state.oauth.selected += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k')
            if state.screen == RunScreen::OAuth && matches!(state.mode, InputMode::Normal) =>
        {
            state.oauth.selected = state.oauth.selected.saturating_sub(1);
        }
        KeyCode::Char('n') if state.screen == RunScreen::OAuth => {
            if state.admin_busy() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Busy",
                        "Wait for the current admin task to finish.",
                    )
                    .with_sticky(false),
                );
            } else if !snap.oauth_surface.services_ready() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Error,
                        "OAuth unavailable",
                        oauth_surface_status(&snap)
                            .unwrap_or("OAuth services unavailable")
                            .to_string(),
                    ),
                );
            } else {
                state.mode = InputMode::OAuthWizard(OAuthUpsertWizard::new());
            }
        }
        KeyCode::Char('x') if state.screen == RunScreen::OAuth => {
            if state.admin_busy() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Busy",
                        "Wait for the current admin task to finish.",
                    )
                    .with_sticky(false),
                );
            } else if let Some(row) = snap.oauth_providers.get(state.oauth.selected) {
                state.mode = InputMode::ConfirmOAuthDisable {
                    entry_id: row.entry_id.clone(),
                };
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Disable pending",
                        format!("Press y to disable {}.", row.entry_id),
                    )
                    .with_action_hint("Press Esc to cancel.")
                    .with_sticky(false),
                );
            } else {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "No provider selected",
                        "Select a provider before disabling it.",
                    )
                    .with_sticky(false),
                );
            }
        }
        KeyCode::Char('y')
            if state.screen == RunScreen::OAuth
                && matches!(state.mode, InputMode::ConfirmOAuthDisable { .. }) =>
        {
            if state.admin_busy() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Busy",
                        "Wait for the current admin task to finish.",
                    )
                    .with_sticky(false),
                );
            } else if let Some(bridge) = deps.admin_bridge {
                let entry_id = match std::mem::replace(&mut state.mode, InputMode::Normal) {
                    InputMode::ConfirmOAuthDisable { entry_id } => entry_id,
                    _ => String::new(),
                };
                submit_inline_admin_job(
                    state,
                    bridge,
                    AdminTaskKind::DisablingOAuthProvider,
                    |c| AdminJob::OauthProviderDisable { corr: c, entry_id },
                );
            } else {
                state.mode = InputMode::Normal;
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Error,
                        "Admin bridge unavailable",
                        "Cannot disable the provider without the admin bridge.",
                    ),
                );
            }
        }
        KeyCode::Char('d') if state.screen == RunScreen::OAuth => {
            if state.admin_busy() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Busy",
                        "Wait for the current admin task to finish.",
                    )
                    .with_sticky(false),
                );
            } else if !snap.oauth_surface.services_ready() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Error,
                        "OAuth unavailable",
                        oauth_surface_status(&snap)
                            .unwrap_or("OAuth services unavailable")
                            .to_string(),
                    ),
                );
            } else if let (Some(bridge), Some(row)) = (
                deps.admin_bridge,
                snap.oauth_providers.get(state.oauth.selected),
            ) {
                let entry_id = row.entry_id.clone();
                let host_state = match deps.host_state {
                    Some(host_state) => host_state,
                    None => {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Error,
                                "OAuth host state unavailable",
                                "The running appliance host state is missing OAuth services.",
                            ),
                        );
                        return false;
                    }
                };
                let catalog = match host_state.oauth_link_catalog() {
                    Some(c) => Arc::clone(c),
                    None => {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Error,
                                "OAuth catalog unavailable",
                                "The running appliance has no OAuth catalog attached.",
                            ),
                        );
                        return false;
                    }
                };
                let storage = match host_state.auth_storage() {
                    Some(s) => Arc::clone(s),
                    None => {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Error,
                                "Auth storage unavailable",
                                "Device authorization cannot run without auth storage.",
                            ),
                        );
                        return false;
                    }
                };
                let reg = host_state.catalog.snapshot();
                match OAuthDeviceScopePickState::try_open(
                    reg.as_ref(),
                    entry_id.clone(),
                    Arc::clone(&catalog),
                    Arc::clone(&storage),
                ) {
                    Ok(Some(pick)) => {
                        state.mode = InputMode::OAuthDeviceScopePick(pick);
                    }
                    Ok(None) => {
                        submit_inline_admin_job(
                            state,
                            bridge,
                            AdminTaskKind::DeviceAuthorization,
                            |c| AdminJob::OAuthDeviceBind {
                                corr: c,
                                entry_id,
                                scopes: vec![],
                                catalog,
                                storage,
                            },
                        );
                    }
                    Err(e) => {
                        set_notice(
                            state,
                            RunNotice::new(
                                NoticeSeverity::Error,
                                "Catalogue lookup failed",
                                format!(
                                    "Could not load OAuth scope catalogue for `{entry_id}`: {e}"
                                ),
                            ),
                        );
                    }
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') if state.screen == RunScreen::Keys => {
            if state.keys.selected + 1 < snap.keys.len() {
                state.keys.selected += 1;
            }
        }
        KeyCode::Up | KeyCode::Char('k') if state.screen == RunScreen::Keys => {
            state.keys.selected = state.keys.selected.saturating_sub(1);
        }
        KeyCode::Char('a') if state.screen == RunScreen::Keys => {
            state.mode = InputMode::AddKeyLabel { buf: String::new() };
        }
        KeyCode::Char('r') if state.screen == RunScreen::Keys => {
            if state.admin_busy() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Busy",
                        "Wait for the current admin task to finish.",
                    )
                    .with_sticky(false),
                );
            } else if let (Some(bridge), Some(cid)) = (deps.admin_bridge, state.resources.config_id)
            {
                if let Some(key_id) = snap.keys.get(state.keys.selected).map(|k| k.key_id) {
                    submit_inline_admin_job(state, bridge, AdminTaskKind::RotatingKey, |c| {
                        AdminJob::RotateApiKey {
                            corr: c,
                            config_id: cid,
                            key_id,
                        }
                    });
                }
            }
        }
        KeyCode::Char('d') if state.screen == RunScreen::Keys => {
            if let Some(key_id) = snap.keys.get(state.keys.selected).map(|k| k.key_id) {
                state.mode = InputMode::ConfirmKeyRevoke { key_id };
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Revoke pending",
                        "Press y to revoke the selected API key.",
                    )
                    .with_action_hint("Press Esc to cancel.")
                    .with_sticky(false),
                );
            }
        }
        KeyCode::Char('y')
            if state.screen == RunScreen::Keys
                && matches!(state.mode, InputMode::ConfirmKeyRevoke { .. }) =>
        {
            let key_id = match std::mem::replace(&mut state.mode, InputMode::Normal) {
                InputMode::ConfirmKeyRevoke { key_id } => key_id,
                _ => return false,
            };
            if state.admin_busy() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Busy",
                        "Wait for the current admin task to finish.",
                    )
                    .with_sticky(false),
                );
            } else if let (Some(bridge), Some(cid)) = (deps.admin_bridge, state.resources.config_id)
            {
                submit_inline_admin_job(state, bridge, AdminTaskKind::RevokingKey, |c| {
                    AdminJob::RevokeApiKey {
                        corr: c,
                        config_id: cid,
                        key_id,
                    }
                });
            }
        }
        KeyCode::Char('p')
            if state.screen == RunScreen::Clients
                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            if snap.keys.get(state.keys.selected).is_none() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "No key selected",
                        "Add a transport API key on the Keys tab before copying the plasm CLI profile.",
                    )
                    .with_sticky(false),
                );
            } else if state.admin_busy() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Busy",
                        "Wait for the current admin task to finish.",
                    )
                    .with_sticky(false),
                );
            } else if let (Some(bridge), Some(cid)) = (deps.admin_bridge, state.resources.config_id)
            {
                if let Some(key_id) = snap.keys.get(state.keys.selected).map(|k| k.key_id) {
                    submit_inline_admin_job(
                        state,
                        bridge,
                        AdminTaskKind::CopyingPlasmCliProfile,
                        |c| AdminJob::RevealApiKey {
                            corr: c,
                            config_id: cid,
                            key_id,
                        },
                    );
                }
            }
        }
        KeyCode::Char('c')
            if (state.screen == RunScreen::Keys || state.screen == RunScreen::Clients)
                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            if state.screen == RunScreen::Clients && snap.keys.get(state.keys.selected).is_none() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "No key selected",
                        "Add a transport API key on the Keys tab before copying client config.",
                    )
                    .with_sticky(false),
                );
            } else if state.admin_busy() {
                set_notice(
                    state,
                    RunNotice::new(
                        NoticeSeverity::Warning,
                        "Busy",
                        "Wait for the current admin task to finish.",
                    )
                    .with_sticky(false),
                );
            } else if let (Some(bridge), Some(cid)) = (deps.admin_bridge, state.resources.config_id)
            {
                if let Some(key_id) = snap.keys.get(state.keys.selected).map(|k| k.key_id) {
                    let kind = if state.screen == RunScreen::Clients {
                        AdminTaskKind::CopyingMcpJson
                    } else {
                        AdminTaskKind::RevealingKey
                    };
                    submit_inline_admin_job(state, bridge, kind, |c| AdminJob::RevealApiKey {
                        corr: c,
                        config_id: cid,
                        key_id,
                    });
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') if state.screen == RunScreen::Status => {
            state.overview.scroll = state.overview.scroll.saturating_add(1);
        }
        KeyCode::Up | KeyCode::Char('k') if state.screen == RunScreen::Status => {
            state.overview.scroll = state.overview.scroll.saturating_sub(1);
        }
        KeyCode::PageDown if state.screen == RunScreen::Status => {
            state.overview.scroll = state.overview.scroll.saturating_add(20);
        }
        KeyCode::PageUp if state.screen == RunScreen::Status => {
            state.overview.scroll = state.overview.scroll.saturating_sub(20);
        }
        KeyCode::Char('g') if state.screen == RunScreen::Status => {
            state.overview.scroll = 0;
        }
        KeyCode::Down | KeyCode::Char('j') if state.screen == RunScreen::Clients => {
            state.clients.scroll = state.clients.scroll.saturating_add(1);
        }
        KeyCode::Up | KeyCode::Char('k') if state.screen == RunScreen::Clients => {
            state.clients.scroll = state.clients.scroll.saturating_sub(1);
        }
        KeyCode::PageDown if state.screen == RunScreen::Clients => {
            state.clients.scroll = state.clients.scroll.saturating_add(20);
        }
        KeyCode::PageUp if state.screen == RunScreen::Clients => {
            state.clients.scroll = state.clients.scroll.saturating_sub(20);
        }
        KeyCode::Char('g') if state.screen == RunScreen::Clients => {
            state.clients.scroll = 0;
        }
        KeyCode::Down | KeyCode::Char('j') if state.screen == RunScreen::Logs => {
            let total = state.logs.lines.len();
            if total > 0 {
                state.logs.cursor = (state.logs.cursor + 1).min(total - 1);
            }
        }
        KeyCode::Up | KeyCode::Char('k') if state.screen == RunScreen::Logs => {
            state.logs.cursor = state.logs.cursor.saturating_sub(1);
        }
        KeyCode::PageDown if state.screen == RunScreen::Logs => {
            let page = 20usize;
            let total = state.logs.lines.len();
            if total > 0 {
                state.logs.cursor = (state.logs.cursor + page).min(total - 1);
            }
        }
        KeyCode::PageUp if state.screen == RunScreen::Logs => {
            let page = 20usize;
            state.logs.cursor = state.logs.cursor.saturating_sub(page);
        }
        KeyCode::Char('g') if state.screen == RunScreen::Logs => {
            state.logs.cursor = 0;
        }
        KeyCode::Char('G') if state.screen == RunScreen::Logs => {
            let total = state.logs.lines.len();
            if total > 0 {
                state.logs.cursor = total - 1;
            }
        }
        _ => {}
    }
    false
}

fn update(state: &mut RunState, msg: UiMsg, deps: &UpdateDeps<'_>) -> bool {
    match msg {
        UiMsg::Tick => {
            state.reset_screen_local_mode();
            false
        }
        UiMsg::Admin(comp) => {
            apply_admin_completion(state, deps.admin_bridge, deps.listen, *comp);
            false
        }
        UiMsg::LogLine(line) => {
            state.logs.lines.push_back(line);
            while state.logs.lines.len() > appliance_log::APPLIANCE_LOG_TAB_MAX_LINES {
                state.logs.lines.pop_front();
                state.logs.cursor = state.logs.cursor.saturating_sub(1);
                state.logs.scroll = state.logs.scroll.saturating_sub(1);
            }
            if state.logs.lines.is_empty() {
                state.logs.cursor = 0;
                state.logs.scroll = 0;
            } else {
                let n = state.logs.lines.len();
                state.logs.cursor = state.logs.cursor.min(n - 1);
            }
            false
        }
        UiMsg::Key(key) => match state.mode {
            InputMode::ApiFilter
            | InputMode::ApiSecretEdit { .. }
            | InputMode::CatalogConnect { .. }
            | InputMode::AddKeyLabel { .. }
            | InputMode::OAuthWizard(_)
            | InputMode::OAuthDeviceScopePick(_) => update_modal_key(state, key, deps),
            InputMode::Normal
            | InputMode::ConfirmOAuthDisable { .. }
            | InputMode::ConfirmKeyRevoke { .. } => update_normal_key(state, key, deps),
        },
    }
}

fn set_run_screen(state: &mut RunState, screen: RunScreen) {
    if state.screen == RunScreen::Status && screen != RunScreen::Status {
        state.overview.scroll = 0;
    }
    if state.screen == RunScreen::Clients && screen != RunScreen::Clients {
        state.clients.scroll = 0;
    }
    state.screen = screen;
}

fn clamp_overview_scroll(scroll: u16, line_count: usize, visible: usize) -> u16 {
    if line_count == 0 || visible == 0 {
        return 0;
    }
    let max_top = line_count.saturating_sub(visible);
    scroll.min(max_top as u16)
}

fn build_overview_lines(
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

fn render_scrollable_panel(
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

fn render_overview_panel(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    lines: &[Line<'static>],
    scroll: u16,
) {
    render_scrollable_panel(frame, area, lines, scroll, "Overview", Some('o'));
}

fn render_running_frame(
    frame: &mut ratatui::Frame<'_>,
    model: &mut RunState,
    host_state: &PlasmHostState,
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    chrome::clear_frame(frame);
    let layout = chrome::split_running_vertical(frame.area());
    let snap = &model.resources.snapshot;
    let tab_titles: Vec<&str> = RunScreen::ALL.iter().map(|s| s.title()).collect();
    let rail_max = layout.tab_rail.width.max(1);
    let rail = chrome::tab_rail_line(model.screen.index(), &tab_titles, listen, rail_max);
    chrome::render_tab_rail(frame, layout.tab_rail, rail);

    let shared_notice = model.notice.as_ref();

    match model.screen {
        RunScreen::Status => {
            let (content_area, notice_area) =
                split_main_notice_area(layout.body, shared_notice.is_some());
            let lines = build_overview_lines(model, snap, host_state, listen);
            render_overview_panel(frame, content_area, &lines, model.overview.scroll);
            if let (Some(area), Some(notice)) = (notice_area, shared_notice) {
                render_notice_panel(frame, area, notice);
            }
        }
        RunScreen::Clients => {
            let (content_area, notice_area) =
                split_main_notice_area(layout.body, shared_notice.is_some());
            let lines = build_clients_panel_lines(listen, snap.keys.get(model.keys.selected));
            render_scrollable_panel(
                frame,
                content_area,
                &lines,
                model.clients.scroll,
                "Clients",
                Some('e'),
            );
            if let (Some(area), Some(notice)) = (notice_area, shared_notice) {
                render_notice_panel(frame, area, notice);
            }
        }
        RunScreen::Apis => {
            let [list_col, right_col] = chrome::split_list_detail(layout.body, 46);
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

            let (detail_area, notice_area) =
                split_main_notice_area(right_col, shared_notice.is_some());
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
            if let (Some(area), Some(notice)) = (notice_area, shared_notice) {
                render_notice_panel(frame, area, notice);
            }
        }
        RunScreen::OAuth => {
            if let InputMode::OAuthWizard(ref wiz) = model.mode {
                let (content_area, notice_area) =
                    split_main_notice_area(layout.body, shared_notice.is_some());
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
                if let (Some(area), Some(notice)) = (notice_area, shared_notice) {
                    render_notice_panel(frame, area, notice);
                }
            } else if let InputMode::OAuthDeviceScopePick(ref pick) = model.mode {
                let (content_area, notice_area) =
                    split_main_notice_area(layout.body, shared_notice.is_some());
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
                if let (Some(area), Some(notice)) = (notice_area, shared_notice) {
                    render_notice_panel(frame, area, notice);
                }
            } else {
                let [split0, split1] = chrome::split_list_detail(layout.body, 40);
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

                let (detail_area, notice_area) =
                    split_main_notice_area(split1, shared_notice.is_some());
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
                if let (Some(area), Some(notice)) = (notice_area, shared_notice) {
                    render_notice_panel(frame, area, notice);
                }
            }
        }
        RunScreen::Keys => {
            let (content_area, notice_area) =
                split_main_notice_area(layout.body, shared_notice.is_some());
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
            if let (Some(area), Some(notice)) = (notice_area, shared_notice) {
                render_notice_panel(frame, area, notice);
            }
        }
        RunScreen::Runs => {
            let (content_area, notice_area) =
                split_main_notice_area(layout.body, shared_notice.is_some());
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
            if let (Some(area), Some(notice)) = (notice_area, shared_notice) {
                render_notice_panel(frame, area, notice);
            }
        }
        RunScreen::Storage => {
            let (content_area, notice_area) =
                split_main_notice_area(layout.body, shared_notice.is_some());
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
            if let (Some(area), Some(notice)) = (notice_area, shared_notice) {
                render_notice_panel(frame, area, notice);
            }
        }
        RunScreen::Logs => {
            let (content_area, notice_area) =
                split_main_notice_area(layout.body, shared_notice.is_some());
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
            if let (Some(area), Some(notice)) = (notice_area, shared_notice) {
                render_notice_panel(frame, area, notice);
            }
        }
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_running_mode(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    host_state: Arc<PlasmHostState>,
    running: Arc<AtomicBool>,
    ui_evt_tx: Option<Sender<UiEvent>>,
    listen: plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    admin_bridge: Option<AdminBridge>,
    policy_bootstrap_detail: Option<PolicyStoreBootstrapDetail>,
    log_rx: Option<crossbeam_channel::Receiver<appliance_log::ApplianceLogEntry>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Signal the async supervisor before the first draw so a full PTY pipe cannot
    // deadlock BOOT→RUN handoff waiting on this frame.
    if let Some(ref tx) = ui_evt_tx {
        if let Err(e) = tx.send(UiEvent::RunEntered) {
            tracing::warn!(
                target: "plasm_appliance_boot",
                "failed to send RunEntered to supervisor: {e}"
            );
        } else {
            tracing::info!(
                target: "plasm_appliance_boot",
                "RUN UI emitted RunEntered to supervisor"
            );
        }
    }
    let mut model = RunState::new();
    model.policy_bootstrap_detail = policy_bootstrap_detail;
    model.resources.snapshot.config_surface = config_surface_from_host(host_state.as_ref());
    if let Some(ref bridge) = admin_bridge {
        enqueue_refresh_if_idle(&mut model, bridge);
    }
    let deps = UpdateDeps {
        admin_bridge: admin_bridge.as_ref(),
        host_state: Some(host_state.as_ref()),
        listen: &listen,
    };

    while running.load(Ordering::SeqCst) {
        if let Some(ref lr) = log_rx {
            for _ in 0..512 {
                match lr.try_recv() {
                    Ok(line) => {
                        let _ = update(&mut model, UiMsg::LogLine(line), &deps);
                    }
                    Err(_) => break,
                }
            }
        }
        if let Some(ref bridge) = admin_bridge {
            while let Ok(comp) = bridge.completions().try_recv() {
                let _ = update(&mut model, UiMsg::Admin(Box::new(comp)), &deps);
            }
        } else if matches!(
            model.resources.snapshot.config_surface,
            McpConfigSurfaceState::PolicyStoreUnavailable {
                reason: PolicyStoreUnavailableReason::NeverAttached
            }
        ) && appliance_services_policy_hint(host_state.as_ref())
        {
            set_notice(
                &mut model,
                RunNotice::new(
                    NoticeSeverity::Info,
                    "Waiting for admin bridge",
                    "Waiting for admin bridge / policy store…",
                )
                .with_sticky(false),
            );
        }
        let _ = update(&mut model, UiMsg::Tick, &deps);

        terminal
            .draw(|frame| render_running_frame(frame, &mut model, host_state.as_ref(), &listen))?;

        for ev in drain_crossterm_events(terminal, Duration::from_millis(120))? {
            match ev {
                Event::Key(key) => {
                    if raw_tty_wants_process_quit(&key) {
                        running.store(false, Ordering::SeqCst);
                        return Ok(());
                    }
                    if update(&mut model, UiMsg::Key(key), &deps) {
                        return Ok(());
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
    }
    Ok(())
}

fn appliance_services_policy_hint(state: &PlasmHostState) -> bool {
    plasm_agent_core::appliance_services::mcp_policy_store_enabled(state)
}

/// Alternate-screen RUN UI only (no BOOT checklist).
#[allow(dead_code)]
pub fn run_control_station(
    state: Arc<PlasmHostState>,
    running: Arc<AtomicBool>,
    listen: plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    enable_raw_mode()?;
    let mut buffer = stdout();
    execute!(buffer, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(buffer);
    let mut terminal = Terminal::new(backend)?;

    let restore_terminal = || {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    };
    let _guard = scopeguard::guard((), |_| restore_terminal());

    let result = run_running_mode(
        &mut terminal,
        state,
        running,
        None,
        listen,
        None,
        None,
        None,
    );

    drop(_guard);
    let _ = terminal.show_cursor();

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use serde_json::json;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn test_listen() -> plasm_agent_core::listen_endpoint::TcpListenEndpoint {
        plasm_agent_core::listen_endpoint::TcpListenEndpoint::new("127.0.0.1", 4100)
    }

    fn listen_on(port: u16) -> plasm_agent_core::listen_endpoint::TcpListenEndpoint {
        plasm_agent_core::listen_endpoint::TcpListenEndpoint::new("127.0.0.1", port)
    }

    fn test_deps<'a>(bridge: Option<&'a AdminBridge>) -> UpdateDeps<'a> {
        static LISTEN: std::sync::OnceLock<plasm_agent_core::listen_endpoint::TcpListenEndpoint> =
            std::sync::OnceLock::new();
        let listen = LISTEN.get_or_init(test_listen);
        UpdateDeps {
            admin_bridge: bridge,
            host_state: None,
            listen,
        }
    }

    fn sample_oauth_provider(
        entry_id: &str,
    ) -> plasm_agent_core::oauth_provider_repository::OauthProviderAppRow {
        plasm_agent_core::oauth_provider_repository::OauthProviderAppRow {
            entry_id: entry_id.into(),
            authorization_endpoint: Some("https://example.test/authorize".into()),
            token_endpoint: Some("https://example.test/token".into()),
            device_authorization_endpoint: Some("https://example.test/device".into()),
            client_id: "client-id".into(),
            client_secret_key: "kv/key".into(),
            enabled: true,
        }
    }

    fn sample_catalog_row(
        entry_id: &str,
        has_public_mode: bool,
        has_api_key: bool,
        has_oauth: bool,
    ) -> McpConfigCatalogRow {
        serde_json::from_value(json!({
            "entry_id": entry_id,
            "label": entry_id,
            "enabled_for_mcp": true,
            "auth_optional": false,
            "has_auth_binding": false,
            "auth_marker": "public",
            "connect_profile": {
                "capability": if has_api_key && has_oauth {
                    "api_key_and_oauth"
                } else if has_api_key {
                    "api_key_only"
                } else if has_oauth {
                    "oauth_only"
                } else {
                    "public"
                },
                "oauth": { "provider_present": has_oauth, "scope_catalog_present": has_oauth },
                "has_public_mode": has_public_mode,
                "has_api_key": has_api_key,
                "has_oauth": has_oauth
            },
            "auth_scheme_summary": "bearer token",
            "api_secret_hosted_kv": "plasm:outbound:v1:test",
            "api_secret_present": false
        }))
        .expect("catalog row json")
    }

    fn buffer_text(buffer: &Buffer) -> String {
        let area = buffer.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn run_screen_wraps_left_and_right() {
        let mut state = RunState::new();
        let deps = test_deps(None);

        assert!(matches!(state.screen, RunScreen::Status));
        assert!(!update(&mut state, UiMsg::Key(key(KeyCode::Left)), &deps));
        assert!(matches!(state.screen, RunScreen::Logs));
        assert!(!update(&mut state, UiMsg::Key(key(KeyCode::Right)), &deps));
        assert!(matches!(state.screen, RunScreen::Status));
    }

    #[test]
    fn api_filter_mode_enters_and_esc_clears() {
        let mut state = RunState::new();
        state.screen = RunScreen::Apis;
        let deps = test_deps(None);

        update(&mut state, UiMsg::Key(key(KeyCode::Char('/'))), &deps);
        assert!(matches!(state.mode, InputMode::ApiFilter));

        update(&mut state, UiMsg::Key(key(KeyCode::Char('g'))), &deps);
        assert_eq!(state.api.filter, "g");

        update(&mut state, UiMsg::Key(key(KeyCode::Esc)), &deps);
        assert!(matches!(state.mode, InputMode::Normal));
        assert!(state.api.filter.is_empty());
    }

    #[test]
    fn add_key_modal_confirms_and_cancels() {
        let mut state = RunState::new();
        state.screen = RunScreen::Keys;
        let deps = test_deps(None);

        update(&mut state, UiMsg::Key(key(KeyCode::Char('a'))), &deps);
        assert!(matches!(state.mode, InputMode::AddKeyLabel { .. }));

        update(&mut state, UiMsg::Key(key(KeyCode::Char('x'))), &deps);
        assert_eq!(state.add_key_label_buf(), Some("x"));

        update(&mut state, UiMsg::Key(key(KeyCode::Esc)), &deps);
        assert!(matches!(state.mode, InputMode::Normal));

        update(&mut state, UiMsg::Key(key(KeyCode::Char('a'))), &deps);
        update(&mut state, UiMsg::Key(key(KeyCode::Char('y'))), &deps);
        update(&mut state, UiMsg::Key(key(KeyCode::Enter)), &deps);
        assert!(matches!(state.mode, InputMode::Normal));
    }

    #[test]
    fn oauth_disable_confirm_cancels_cleanly() {
        let mut state = RunState::new();
        state.screen = RunScreen::OAuth;
        state.resources.snapshot.oauth_providers = vec![sample_oauth_provider("github")];
        let deps = test_deps(None);

        update(&mut state, UiMsg::Key(key(KeyCode::Char('x'))), &deps);
        assert!(matches!(state.mode, InputMode::ConfirmOAuthDisable { .. }));

        update(&mut state, UiMsg::Key(key(KeyCode::Esc)), &deps);
        assert!(matches!(state.mode, InputMode::Normal));
        let notice = state.notice.expect("cancel notice");
        assert_eq!(notice.title, "Disable cancelled");
        assert_eq!(
            notice.summary,
            "Dismissed the provider disable confirmation."
        );
    }

    #[test]
    fn copy_notice_never_echoes_secret() {
        let secret = "plasm-secret-value";
        let ok_notice = copy_notice("API key secret copied", "copy failed", Ok(()));
        let err_notice = copy_notice(
            "API key secret copied",
            "copy failed",
            Err("clipboard missing".into()),
        );

        assert!(!ok_notice.summary.contains(secret));
        assert!(err_notice.details.iter().all(|line| !line.contains(secret)));
        assert_eq!(ok_notice.title, "API key secret copied");
        assert_eq!(err_notice.title, "copy failed");
    }

    #[test]
    fn auth_labels_show_supported_and_current_config() {
        let mut snap = UiSnapshot::default();
        let mut row = sample_catalog_row("github", false, true, true);
        row.api_secret_present = true;
        snap.oauth_providers = vec![sample_oauth_provider("github")];
        snap.oauth_binding_hints = vec!["kv ok · exp 123".into()];

        assert_eq!(auth_kind_label(&row), "api key+oauth");
        assert!(current_auth_config_label(&row, &snap).contains("api key set"));
        assert!(current_auth_config_label(&row, &snap).contains("oauth provider ready"));
    }

    #[test]
    fn unlabeled_keys_use_fingerprint_not_key_id() {
        let row = McpConfigApiKeyRow {
            key_id: Uuid::nil(),
            fingerprint: "deadbeefcafebabe".into(),
            label: None,
        };

        assert_eq!(api_key_row_label(&row), "(unnamed · fp:deadbeef)");
        assert_eq!(api_key_row_copy_line(&row), "(unnamed · fp:deadbeef)");
    }

    #[test]
    fn storage_backend_summary_is_actionable() {
        assert_eq!(
            storage_backend_summary(true, None),
            (
                "Embedded Postgres",
                "This appliance is managing its own local PostgreSQL 15 cluster.".into()
            )
        );
        assert_eq!(
            storage_backend_summary(
                false,
                Some("PLASM_EMBEDDED_POSTGRES=0 disables embedded Postgres")
            ),
            (
                "External / disabled Postgres",
                "PLASM_EMBEDDED_POSTGRES=0 disables embedded Postgres".into()
            )
        );
    }

    #[test]
    fn stale_refresh_completion_is_ignored() {
        let mut state = RunState::new();
        state.resources.snapshot.config_surface = McpConfigSurfaceState::Ready {
            summary_name: "old".into(),
            summary_status: "old-status".into(),
            enabled_api_count: 0,
            key_count: 0,
        };
        state.resources.admin.start_refresh(7);
        let deps = test_deps(None);

        let data = RefreshedUiData {
            config_surface: McpConfigSurfaceState::Ready {
                summary_name: "new".into(),
                summary_status: "ready".into(),
                enabled_api_count: 1,
                key_count: 0,
            },
            config_id: Some(Uuid::nil()),
            catalog_rows: Vec::new(),
            keys: Vec::new(),
            db_allowed: HashSet::new(),
            oauth_providers: Vec::new(),
            oauth_binding_hints: Vec::new(),
            oauth_surface: OAuthSurfaceState::CatalogUnavailable,
        };

        update(
            &mut state,
            UiMsg::Admin(Box::new(AdminCompletion::RefreshFull { corr: 6, data })),
            &deps,
        );

        assert!(matches!(
            state.resources.snapshot.config_surface,
            McpConfigSurfaceState::Ready { ref summary_name, .. } if summary_name == "old"
        ));
        assert_eq!(state.resources.admin.pending_refresh_corr(), Some(7));
    }

    #[test]
    fn esc_dismisses_transient_notice() {
        let mut state = RunState::new();
        state.notice = Some(
            RunNotice::new(NoticeSeverity::Success, "Saved", "Saved changes.").with_sticky(false),
        );
        let deps = test_deps(None);

        update(&mut state, UiMsg::Key(key(KeyCode::Esc)), &deps);

        assert!(state.notice.is_none());
    }

    #[test]
    fn device_bind_error_notice_classifies_disabled_device_flow() {
        let notice = device_bind_error_notice(
            "github",
            "OAuth device authorization failed: HTTP 400 Bad Request: device_flow_disabled",
        );

        assert_eq!(notice.title, "Bind failed");
        assert!(notice
            .summary
            .contains("github rejected device authorization"));
        assert!(notice
            .action_hint
            .as_deref()
            .unwrap_or_default()
            .contains("Enable device flow"));
        assert_eq!(
            notice.details,
            vec![
                "OAuth device authorization failed: HTTP 400 Bad Request: device_flow_disabled"
                    .to_string()
            ]
        );
    }

    #[test]
    fn device_bind_started_completion_surfaces_url_and_code_before_finish() {
        let mut state = RunState::new();
        state.screen = RunScreen::OAuth;
        state.resources.snapshot.oauth_providers = vec![sample_oauth_provider("github")];
        state
            .resources
            .admin
            .start_inline(42, AdminTaskKind::DeviceAuthorization);

        apply_admin_completion(
            &mut state,
            None,
            &test_listen(),
            AdminCompletion::OAuthDeviceBindStarted {
                corr: 42,
                prompt: crate::appliance_oauth_admin::DeviceBindPrompt {
                    user_code: "ABCD-EFGH".into(),
                    verification_uri: "https://github.com/login/device".into(),
                    verification_uri_complete: Some(
                        "https://github.com/login/device?user_code=ABCD-EFGH".into(),
                    ),
                    expires_in_secs: 900,
                    poll_interval_secs: 5,
                },
            },
        );

        let notice = state.notice.expect("bind started notice");
        assert_eq!(notice.title, "Bind started");
        assert!(notice.summary.contains("github"));
        assert!(notice
            .details
            .iter()
            .any(|line| line.contains("github.com/login/device")));
        assert!(notice.details.iter().any(|line| line.contains("ABCD-EFGH")));
        assert_eq!(state.resources.admin.pending_inline_corr(), Some(42));
    }

    #[test]
    fn api_key_shortcut_opens_secret_modal_for_supported_entry() {
        let mut state = RunState::new();
        state.screen = RunScreen::Apis;
        state.resources.snapshot.catalog_rows =
            vec![sample_catalog_row("github", false, true, false)];
        state.api.filtered_ix = vec![0];
        let deps = test_deps(None);

        update(&mut state, UiMsg::Key(key(KeyCode::Char('a'))), &deps);

        assert!(matches!(state.mode, InputMode::ApiSecretEdit { .. }));
        let notice = state.notice.expect("api key notice");
        assert_eq!(notice.title, "Set API key");
    }

    #[test]
    fn apply_oauth_binding_to_snapshot_updates_oauth_and_api_rows() {
        let mut state = RunState::new();
        state.resources.snapshot.oauth_providers = vec![sample_oauth_provider("github")];
        state.resources.snapshot.oauth_binding_hints = vec!["no binding".into()];
        state.resources.snapshot.catalog_rows = vec![serde_json::from_value(json!({
            "entry_id": "github",
            "label": "GitHub",
            "enabled_for_mcp": true,
            "auth_optional": false,
            "has_auth_binding": false,
            "auth_marker": "missing_binding",
            "connect_profile": {
                "capability": "oauth_only",
                "oauth": { "provider_present": true, "scope_catalog_present": true },
                "has_public_mode": false,
                "has_api_key": false,
                "has_oauth": true
            }
        }))
        .expect("catalog row json")];

        apply_oauth_binding_to_snapshot(&mut state, "github");

        assert_eq!(
            state.resources.snapshot.oauth_binding_hints,
            vec!["binding updated — refreshing…"]
        );
        assert!(state.resources.snapshot.catalog_rows[0].has_auth_binding);
        assert_eq!(
            state.resources.snapshot.catalog_rows[0].auth_marker,
            McpCatalogAuthMarker::RequiresConnect
        );
    }

    fn line_text(line: &ratatui::text::Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn run_footer_includes_quit_hint() {
        let global = [
            chrome::FooterItem::new("←/→", "tab"),
            chrome::FooterItem::new("q", "quit"),
        ];
        let text = line_text(&chrome::footer_line(&global, &[], None, None));
        assert!(text.contains("q: quit"));
    }

    #[test]
    fn mcp_client_json_config_has_streamable_http_shape() {
        let json = mcp_client_json_config(&test_listen(), None).expect("json");
        let v: serde_json::Value = serde_json::from_str(json.trim()).expect("parse");
        let plasm = v
            .get("mcpServers")
            .and_then(|m| m.get("plasm"))
            .expect("plasm entry");
        assert_eq!(
            plasm.get("type").and_then(|t| t.as_str()),
            Some("streamableHttp")
        );
        assert_eq!(
            plasm.get("url").and_then(|u| u.as_str()),
            Some("http://127.0.0.1:4100/mcp")
        );
        assert_eq!(
            plasm
                .get("headers")
                .and_then(|h| h.get("Authorization"))
                .and_then(|a| a.as_str()),
            Some(MCP_JSON_PLACEHOLDER_BEARER)
        );
    }

    #[test]
    fn mcp_client_json_display_never_includes_raw_secret() {
        let secret = "plasm_test_secret_abc123xyz";
        let display = mcp_client_json_config(&test_listen(), None).expect("display");
        assert!(!display.contains(secret));
        let with_secret = mcp_client_json_config(&test_listen(), Some(secret)).expect("copy");
        assert!(with_secret.contains(secret));
        assert!(with_secret.contains("Bearer plasm_test_secret"));
    }

    #[test]
    fn clients_tab_footer_includes_copy_config() {
        let mut state = RunState::new();
        state.screen = RunScreen::Clients;
        let items = screen_footer_items(&state);
        assert!(items.iter().any(|i| i.key == "c" && i.desc.contains("MCP")));
        assert!(items
            .iter()
            .any(|i| i.key == "p" && i.desc.contains("plasm CLI")));
    }

    #[test]
    fn plasm_cli_profile_json_has_server_and_api_key() {
        let listen = listen_on(3001);
        let json = plasm_cli_profile_json_config(&listen, None).expect("json");
        let v: serde_json::Value = serde_json::from_str(json.trim()).expect("parse");
        assert_eq!(
            v.get("server").and_then(|s| s.as_str()),
            Some("http://127.0.0.1:3001")
        );
        assert_eq!(
            v.get("api_key").and_then(|s| s.as_str()),
            Some(PLASM_CLI_PLACEHOLDER_API_KEY)
        );
    }

    #[test]
    fn plasm_cli_profile_display_never_includes_raw_secret() {
        let secret = "plasm_test_secret_abc123xyz";
        let listen = listen_on(3001);
        let display = plasm_cli_profile_json_config(&listen, None).expect("display");
        assert!(!display.contains(secret));
        let with_secret = plasm_cli_profile_json_config(&listen, Some(secret)).expect("copy");
        assert!(with_secret.contains(secret));
    }

    #[test]
    fn apis_filter_bar_heading() {
        let text = line_text(&chrome::filter_bar_line("Filter catalogues (/)", "", false));
        assert!(text.contains("Filter catalogues"));
    }

    #[test]
    fn keys_tab_footer_includes_add() {
        let mut state = RunState::new();
        state.screen = RunScreen::Keys;
        let items = screen_footer_items(&state);
        assert!(items.iter().any(|i| i.key == "a" && i.desc.contains("add")));
    }

    #[test]
    fn oauth_wizard_esc_sets_cancel_notice() {
        let mut state = RunState::new();
        state.screen = RunScreen::OAuth;
        state.resources.snapshot.oauth_providers = vec![sample_oauth_provider("github")];
        state.resources.snapshot.oauth_surface = OAuthSurfaceState::Ready;
        let deps = test_deps(None);

        update(&mut state, UiMsg::Key(key(KeyCode::Char('n'))), &deps);
        assert!(matches!(state.mode, InputMode::OAuthWizard(_)));

        update(&mut state, UiMsg::Key(key(KeyCode::Esc)), &deps);
        assert!(matches!(state.mode, InputMode::Normal));
        let notice = state.notice.expect("wizard cancel notice");
        assert_eq!(notice.title, "OAuth wizard cancelled");
    }

    fn min_test_host_state() -> PlasmHostState {
        use plasm_agent_core::http::{build_plasm_host_state, PlasmHostBootstrap};
        use plasm_core::discovery::InMemoryCgsRegistry;
        use plasm_core::loader::load_schema_dir;
        use plasm_runtime::{ExecutionConfig, ExecutionEngine, ExecutionMode};
        use std::path::Path;
        use std::sync::Arc;

        let dir =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/schemas/overshow_tools");
        let cgs = Arc::new(load_schema_dir(&dir).expect("overshow_tools"));
        let reg = InMemoryCgsRegistry::from_pairs(vec![(
            "overshow".into(),
            "Overshow".into(),
            vec!["demo".into()],
            cgs.clone(),
        )]);
        let engine = ExecutionEngine::new(ExecutionConfig::default()).expect("engine");
        build_plasm_host_state(PlasmHostBootstrap {
            engine,
            mode: ExecutionMode::Live,
            registry: Arc::new(reg),
            catalog_bootstrap: plasm_agent_core::server_state::CatalogBootstrap::Fixed,
            plugin_manager: None,
            incoming_auth: None,
            run_artifacts: Arc::new(plasm_agent_core::run_artifacts::RunArtifactStore::memory()),
            session_graph_persistence: None,
            oss_local_filesystem_defaults: false,
        })
    }

    #[test]
    fn overview_unavailable_long_detail_no_garbled_overlap() {
        use plasm_agent_core::mcp_config_repository::McpConfigRepositoryError;

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        let host = rt.block_on(async { min_test_host_state() });

        let mut model = RunState::new();
        model.policy_bootstrap_detail = Some(PolicyStoreBootstrapDetail::MigrateFailed(
            McpConfigRepositoryError::PostMigrateSchemaMissing,
        ));
        model.resources.snapshot.config_surface = McpConfigSurfaceState::PolicyStoreUnavailable {
            reason: PolicyStoreUnavailableReason::NeverAttached,
        };
        let lines =
            build_overview_lines(&model, &model.resources.snapshot, &host, &listen_on(3001));
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");
        assert!(!rendered.contains("enabledts"));
        assert!(rendered.contains("Trace hub:"));
        assert!(rendered.contains("project_mcp_* connect/migrate failed"));

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                render_overview_panel(frame, frame.area(), &lines, 0);
            })
            .expect("draw overview");
        let buffer_text = buffer_text(terminal.backend().buffer());
        assert!(!buffer_text.contains("enabledts"));
        assert!(buffer_text.contains("Trace hub"));
    }

    #[test]
    fn notice_panel_wraps_long_bind_failure() {
        let notice = device_bind_error_notice(
            "github",
            "OAuth device authorization failed: HTTP 400 Bad Request: device_flow_disabled and a very long provider explanation that should wrap cleanly inside the notice panel",
        );
        let backend = TestBackend::new(48, 12);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| {
                render_notice_panel(frame, frame.area(), &notice);
            })
            .expect("draw notice panel");

        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("Bind failed"));
        assert!(rendered.contains("ERROR"));
        assert!(rendered.contains("device_flow_disabled"));
        assert!(rendered.contains("Enable"));
        assert!(rendered.contains("OAuth app"));
    }

    #[test]
    fn format_api_catalogue_row_respects_display_width() {
        use unicode_width::UnicodeWidthStr;
        let row = format_api_catalogue_row(
            true,
            false,
            "cloudflare",
            "api key+oauth · unconfigured service-local / default / default",
            32,
        );
        assert!(line_text(&row).width() <= 32);
    }

    #[test]
    fn run_tab_rail_visible_on_first_draw_without_keypress() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                chrome::clear_frame(frame);
                let layout = chrome::split_running_vertical(frame.area());
                let titles: Vec<&str> = RunScreen::ALL.iter().map(|s| s.title()).collect();
                let rail = chrome::tab_rail_line(
                    2,
                    &titles,
                    &listen_on(8080),
                    layout.tab_rail.width.max(1),
                );
                chrome::render_tab_rail(frame, layout.tab_rail, rail);
            })
            .expect("draw tab rail");

        let rendered = buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("[APIs]"));
        assert!(rendered.contains("127.0.0.1:8080"));
    }
}
