//! Admin bridge task queue and completions

use super::*;

pub(crate) fn alloc_admin_corr(state: &mut RunState) -> AdminCorr {
    state.resources.admin.next_corr = state.resources.admin.next_corr.wrapping_add(1).max(1);
    state.resources.admin.next_corr
}

pub(crate) fn enqueue_refresh_if_idle(state: &mut RunState, bridge: &AdminBridge) {
    if state.resources.admin.refresh.is_some() {
        return;
    }
    enqueue_refresh_force(state, bridge);
}

/// Queue a full snapshot refresh and supersede any in-flight refresh correlation (stale completions ignored).
pub(crate) fn enqueue_refresh_force(state: &mut RunState, bridge: &AdminBridge) {
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

pub(crate) fn submit_inline_admin_job(
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

pub(crate) fn apply_refreshed_ui_data(state: &mut RunState, data: RefreshedUiData) {
    state.resources.config_id = data.config_id;
    state.resources.snapshot.config_surface = data.config_surface;
    state.resources.snapshot.catalog_rows = data.catalog_rows;
    state.resources.snapshot.keys = data.keys;
    state.resources.snapshot.db_allowed = data.db_allowed;
    state.resources.snapshot.oauth_providers = data.oauth_providers;
    state.resources.snapshot.oauth_binding_hints = data.oauth_binding_hints;
    state.resources.snapshot.oauth_surface = data.oauth_surface;
}

pub(crate) fn apply_admin_completion(
    state: &mut RunState,
    bridge: Option<&AdminBridge>,
    listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
    clipboard: &ClipboardService,
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
                            clipboard.copy_text(&raw),
                        ),
                    ),
                    (AdminTaskKind::CopyingMcpJson, Ok(raw)) => {
                        match mcp_client_json_config(listen, Some(&raw)) {
                            Ok(json) => set_notice(
                                state,
                                copy_notice(
                                    "MCP client config copied",
                                    "MCP client config copy failed",
                                    clipboard.copy_text(&json),
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
                                    clipboard.copy_text(&json),
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
