//! Transient notices and OAuth/API apply helpers

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeSeverity {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunNotice {
    pub(crate) severity: NoticeSeverity,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) details: Vec<String>,
    pub(crate) action_hint: Option<String>,
    pub(crate) sticky: bool,
}

impl RunNotice {
    pub(crate) fn new(
        severity: NoticeSeverity,
        title: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
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

    pub(crate) fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }

    pub(crate) fn with_action_hint(mut self, hint: impl Into<String>) -> Self {
        self.action_hint = Some(hint.into());
        self
    }

    pub(crate) fn with_sticky(mut self, sticky: bool) -> Self {
        self.sticky = sticky;
        self
    }

    pub(crate) fn severity_label(&self) -> &'static str {
        match self.severity {
            NoticeSeverity::Info => "INFO",
            NoticeSeverity::Success => "SUCCESS",
            NoticeSeverity::Warning => "WARNING",
            NoticeSeverity::Error => "ERROR",
        }
    }

    pub(crate) fn heading_style(&self) -> Style {
        match self.severity {
            NoticeSeverity::Info => run_title_style(),
            NoticeSeverity::Success => api_toggle_on_style(),
            NoticeSeverity::Warning => warn_emphasis_style(),
            NoticeSeverity::Error => err_emphasis_style(),
        }
    }

    pub(crate) fn block_title(&self) -> String {
        self.title.clone()
    }

    pub(crate) fn lines(&self) -> Vec<Line<'static>> {
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

pub(crate) fn set_notice(state: &mut RunState, notice: RunNotice) {
    state.notice = Some(notice);
}

pub(crate) fn dismiss_transient_notice(state: &mut RunState) -> bool {
    if state.notice.as_ref().is_some_and(|notice| !notice.sticky) {
        state.notice = None;
        return true;
    }
    false
}

pub(crate) fn split_main_notice_area(area: Rect, show_notice: bool) -> (Rect, Option<Rect>) {
    chrome::split_with_notice(area, show_notice)
}

pub(crate) fn sync_log_cursor_scroll(logs: &mut LogState, visible: usize) {
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

pub(crate) fn screen_footer_items(model: &RunState) -> Vec<chrome::FooterItem> {
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

pub(crate) fn render_notice_panel(frame: &mut ratatui::Frame<'_>, area: Rect, notice: &RunNotice) {
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

pub(crate) fn selected_oauth_entry_id(state: &RunState) -> Option<&str> {
    state
        .resources
        .snapshot
        .oauth_providers
        .get(state.oauth.selected)
        .map(|row| row.entry_id.as_str())
}

pub(crate) fn selected_api_row<'a>(
    state: &'a RunState,
    snap: &'a UiSnapshot,
) -> Option<&'a McpConfigCatalogRow> {
    let row_ix = *state.api.filtered_ix.get(state.api.selected)?;
    snap.catalog_rows.get(row_ix)
}

pub(crate) fn auth_kind_label(row: &McpConfigCatalogRow) -> String {
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

pub(crate) fn oauth_provider_summary(snap: &UiSnapshot, entry_id: &str) -> Option<String> {
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

pub(crate) fn current_auth_config_label(row: &McpConfigCatalogRow, snap: &UiSnapshot) -> String {
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
pub(crate) fn format_api_catalogue_row(
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
pub(crate) fn clip_list_row_plain(parts: &str, inner_cols: u16) -> String {
    log_render::clip_line_display(parts, inner_cols.max(1))
}

/// Drain crossterm events until idle; resize updates terminal geometry.
pub(crate) fn drain_crossterm_events(
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

pub(crate) fn api_secret_notice(entry_id: &str) -> RunNotice {
    RunNotice::new(
        NoticeSeverity::Success,
        "API key stored",
        format!("Stored the API secret for {entry_id}."),
    )
    .with_action_hint("Requests for this catalogue can now resolve the hosted secret locally.")
    .with_sticky(false)
}

pub(crate) fn apply_oauth_binding_to_snapshot(state: &mut RunState, entry_id: &str) {
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

pub(crate) fn apply_api_secret_to_snapshot(state: &mut RunState, entry_id: &str) {
    for row in &mut state.resources.snapshot.catalog_rows {
        if row.entry_id == entry_id {
            row.api_secret_present = true;
        }
    }
}

pub(crate) fn select_oauth_config_from_api(state: &mut RunState, entry_id: &str) {
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

pub(crate) fn device_bind_started_notice(
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

pub(crate) fn device_bind_success_notice(
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

pub(crate) fn device_bind_error_notice(entry_id: &str, raw_error: &str) -> RunNotice {
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

pub(crate) fn copy_notice(
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
