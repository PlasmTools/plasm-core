# plasm-server TUI test inventory

| Surface | Code | Unit (`tui/mod.rs` tests) | Integration |
|--------|------|---------------------------|-------------|
| BOOT checklist / RUN handoff | `boot.rs`, `main.rs` | — | `appliance_headless_boot` (diag); PTY smoke waits for RUN footer |
| Frame clear + resize | `tui/chrome.rs`, `tui/mod.rs`, `boot.rs` | `running_layout_reserves_tab_rail_and_footer` (chrome), `run_tab_rail_visible_on_first_draw_without_keypress` | PTY smoke |
| RUN shell / quit `q` | `tui/mod.rs` | `run_footer_includes_quit_hint` | `plasm-server-pty-tests` `tui_pty_quit_smoke` |
| Tab navigation | `tui/mod.rs` | `run_screen_wraps_left_and_right` | PTY smoke (implicit: footer visible) |
| Status — listeners | `tui/mod.rs` | — | headless diag |
| Clients — MCP JSON display + copy | `tui/mod.rs` | `mcp_client_json_config_has_streamable_http_shape`, `clients_tab_footer_includes_copy_config` | — |
| APIs — filter bar + row clip | `tui/mod.rs`, `tui/log_render.rs` | `apis_filter_bar_heading`, `api_filter_mode_enters_and_esc_clears`, `format_api_catalogue_row_respects_display_width` | — |
| OAuth — wizard Esc cancel | `tui/mod.rs` | `oauth_wizard_esc_sets_cancel_notice`, `oauth_disable_confirm_cancels_cleanly` | — |
| Keys — add / footer | `tui/mod.rs` | `keys_tab_footer_includes_add`, `add_key_modal_confirms_and_cancels` | — |
| Discovery — semantic auto-seed | `tui/screens/discovery.rs`, `discovery_bootstrap.rs` | — | CLI `plasm-server discovery status` |
| Bootstrap logs during RUN | `main.rs`, `tui/mod.rs` | — (tracing → Logs tab only) | — |

CI: `bash scripts/appliance-tui-pty-tests.sh` (headless gate + one PTY quit smoke).
