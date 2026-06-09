//! Ratatui control station — Your MCP–oriented tabs over [`PlasmHostState`] (no loopback HTTP).

use std::io::{self, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use plasm_agent_core::server_state::PlasmHostState;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

pub(crate) mod chrome;
pub(crate) mod log_render;
mod oauth_device_scope_pick;
mod prelude;

pub(crate) use prelude::*;

mod admin;
mod catalog;
mod helpers;
mod input;
mod notice;
mod render;
mod screens;
mod state;
mod station;
mod styles;

pub(crate) use admin::*;
pub(crate) use catalog::*;
pub(crate) use helpers::*;
pub(crate) use input::*;
pub(crate) use notice::*;
pub(crate) use render::{
    build_overview_lines, render_overview_panel, render_running_frame, render_scrollable_panel,
};
pub(crate) use state::*;
pub(crate) use station::run_running_mode;
pub(crate) use styles::*;

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
