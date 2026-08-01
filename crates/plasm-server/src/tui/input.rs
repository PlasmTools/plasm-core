//! Keyboard input and update loop

use super::*;

pub(crate) struct UpdateDeps<'a> {
    pub(crate) admin_bridge: Option<&'a AdminBridge>,
    pub(crate) host_state: Option<&'a PlasmHostState>,
    pub(crate) listen: &'a plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    pub(crate) clipboard: ClipboardService,
}
pub(crate) fn update_modal_key(state: &mut RunState, key: KeyEvent, deps: &UpdateDeps<'_>) -> bool {
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
        InputMode::DiscoveryOpenRouterKey { buf } => match key.code {
            KeyCode::Enter => {
                let secret = buf.trim().to_string();
                state.mode = InputMode::Normal;
                if secret.is_empty() {
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Warning,
                            "Empty key",
                            "OpenRouter API key must not be empty.",
                        )
                        .with_sticky(false),
                    );
                } else if let Err(e) = crate::discovery_bootstrap::set_openrouter_api_key(&secret) {
                    set_notice(
                        state,
                        RunNotice::new(NoticeSeverity::Error, "Save failed", e),
                    );
                } else {
                    set_notice(
                        state,
                        RunNotice::new(
                            NoticeSeverity::Info,
                            "OpenRouter key saved",
                            "Semantic auto-seed can call OpenRouter when enabled.",
                        )
                        .with_sticky(false),
                    );
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

pub(crate) fn update_normal_key(
    state: &mut RunState,
    key: KeyEvent,
    deps: &UpdateDeps<'_>,
) -> bool {
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
                    deps.clipboard.copy_text(&url),
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
                        deps.clipboard.copy_text(&line),
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
        KeyCode::Char('e') if state.screen == RunScreen::Discovery => {
            if matches!(state.mode, InputMode::Normal) {
                let enabled =
                    !crate::discovery_bootstrap::current_state().semantic_auto_seed_enabled;
                match crate::discovery_bootstrap::set_semantic_auto_seed_enabled(enabled) {
                    Ok(()) => {
                        set_notice(
                            state,
                            RunNotice::new(
                                if enabled {
                                    NoticeSeverity::Info
                                } else {
                                    NoticeSeverity::Warning
                                },
                                if enabled {
                                    "Semantic auto-seed enabled"
                                } else {
                                    "Semantic auto-seed disabled"
                                },
                                if enabled {
                                    "Intent-only plasm_context new sessions will route via OpenRouter."
                                } else {
                                    "Pass explicit seeds on plasm_context session_mode new."
                                },
                            )
                            .with_sticky(false),
                        );
                    }
                    Err(e) => {
                        set_notice(
                            state,
                            RunNotice::new(NoticeSeverity::Error, "Toggle failed", e),
                        );
                    }
                }
            }
        }
        KeyCode::Char('k') if state.screen == RunScreen::Discovery => {
            if matches!(state.mode, InputMode::Normal) {
                state.mode = InputMode::DiscoveryOpenRouterKey { buf: String::new() };
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

pub(crate) fn update(state: &mut RunState, msg: UiMsg, deps: &UpdateDeps<'_>) -> bool {
    match msg {
        UiMsg::Tick => {
            state.reset_screen_local_mode();
            false
        }
        UiMsg::Admin(comp) => {
            apply_admin_completion(
                state,
                deps.admin_bridge,
                deps.listen,
                &deps.clipboard,
                *comp,
            );
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
            | InputMode::DiscoveryOpenRouterKey { .. }
            | InputMode::OAuthWizard(_)
            | InputMode::OAuthDeviceScopePick(_) => update_modal_key(state, key, deps),
            InputMode::Normal
            | InputMode::ConfirmOAuthDisable { .. }
            | InputMode::ConfirmKeyRevoke { .. } => update_normal_key(state, key, deps),
        },
    }
}
pub(crate) fn set_run_screen(state: &mut RunState, screen: RunScreen) {
    if state.screen == RunScreen::Status && screen != RunScreen::Status {
        state.overview.scroll = 0;
    }
    if state.screen == RunScreen::Clients && screen != RunScreen::Clients {
        state.clients.scroll = 0;
    }
    state.screen = screen;
}
