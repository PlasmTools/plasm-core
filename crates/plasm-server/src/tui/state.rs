//! RunState, screens, and input modes

use super::*;

#[derive(Clone, Default)]
pub(crate) struct UiSnapshot {
    pub(crate) config_surface: McpConfigSurfaceState,
    pub(crate) catalog_rows: Vec<McpConfigCatalogRow>,
    pub(crate) keys: Vec<McpConfigApiKeyRow>,
    pub(crate) db_allowed: HashSet<String>,
    pub(crate) oauth_providers:
        Vec<plasm_agent_core::oauth_provider_repository::OauthProviderAppRow>,
    pub(crate) oauth_binding_hints: Vec<String>,
    pub(crate) oauth_surface: OAuthSurfaceState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunScreen {
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
    pub(crate) const ALL: [Self; 8] = [
        Self::Status,
        Self::Clients,
        Self::Apis,
        Self::OAuth,
        Self::Keys,
        Self::Runs,
        Self::Storage,
        Self::Logs,
    ];

    pub(crate) fn title(self) -> &'static str {
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

    pub(crate) fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    pub(crate) fn prev(self) -> Self {
        Self::ALL[self.index().checked_sub(1).unwrap_or(Self::ALL.len() - 1)]
    }

    pub(crate) fn index(self) -> usize {
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
pub(crate) enum InputMode {
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
pub(crate) struct ApiState {
    pub(crate) selected: usize,
    /// Indices into `snapshot.catalog_rows` after filter.
    pub(crate) filtered_ix: Vec<usize>,
    pub(crate) filter: String,
    pub(crate) staged_allowed: Option<HashSet<String>>,
}

#[derive(Default)]
pub(crate) struct OAuthState {
    pub(crate) selected: usize,
}

#[derive(Default)]
pub(crate) struct KeysState {
    pub(crate) selected: usize,
}

#[derive(Default)]
pub(crate) struct LogState {
    pub(crate) lines: VecDeque<appliance_log::ApplianceLogEntry>,
    pub(crate) scroll: usize,
    /// Selected line index; viewport scroll is synced in [`render_running_frame`].
    pub(crate) cursor: usize,
}

#[derive(Default)]
pub(crate) struct OverviewState {
    pub(crate) scroll: u16,
}

#[derive(Default)]
pub(crate) struct ClientsState {
    pub(crate) scroll: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdminTaskKind {
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
    pub(crate) fn label(self) -> &'static str {
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
pub(crate) struct PendingAdminTask {
    pub(crate) corr: AdminCorr,
    pub(crate) kind: AdminTaskKind,
    pub(crate) started_at: Instant,
}

#[derive(Default)]
pub(crate) struct AdminSyncState {
    /// Monotonic correlation id for async admin jobs.
    pub(crate) next_corr: AdminCorr,
    pub(crate) refresh: Option<PendingAdminTask>,
    pub(crate) inline: Option<PendingAdminTask>,
}

impl AdminSyncState {
    pub(crate) fn pending_refresh_corr(&self) -> Option<AdminCorr> {
        self.refresh.map(|task| task.corr)
    }

    pub(crate) fn pending_inline_corr(&self) -> Option<AdminCorr> {
        self.inline.map(|task| task.corr)
    }

    pub(crate) fn start_refresh(&mut self, corr: AdminCorr) {
        self.refresh = Some(PendingAdminTask {
            corr,
            kind: AdminTaskKind::Refreshing,
            started_at: Instant::now(),
        });
    }

    pub(crate) fn start_inline(&mut self, corr: AdminCorr, kind: AdminTaskKind) {
        self.inline = Some(PendingAdminTask {
            corr,
            kind,
            started_at: Instant::now(),
        });
    }

    pub(crate) fn finish_refresh(&mut self, corr: AdminCorr) -> bool {
        if self.pending_refresh_corr() == Some(corr) {
            self.refresh = None;
            return true;
        }
        false
    }

    pub(crate) fn finish_inline(&mut self, corr: AdminCorr) -> Option<AdminTaskKind> {
        if self.pending_inline_corr() == Some(corr) {
            return self.inline.take().map(|task| task.kind);
        }
        None
    }

    pub(crate) fn busy_task(&self) -> Option<PendingAdminTask> {
        self.inline.or(self.refresh)
    }
}

#[derive(Default)]
pub(crate) struct ResourceState {
    pub(crate) snapshot: UiSnapshot,
    pub(crate) config_id: Option<Uuid>,
    pub(crate) admin: AdminSyncState,
}

pub(crate) struct RunState {
    pub(crate) screen: RunScreen,
    pub(crate) mode: InputMode,
    pub(crate) api: ApiState,
    pub(crate) oauth: OAuthState,
    pub(crate) keys: KeysState,
    pub(crate) logs: LogState,
    pub(crate) resources: ResourceState,
    pub(crate) notice: Option<RunNotice>,
    pub(crate) overview: OverviewState,
    pub(crate) clients: ClientsState,
    pub(crate) policy_bootstrap_detail: Option<PolicyStoreBootstrapDetail>,
}

impl RunState {
    pub(crate) fn new() -> Self {
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

    pub(crate) fn recompute_filter(&mut self, rows: &[McpConfigCatalogRow]) {
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

    pub(crate) fn add_key_label_buf(&self) -> Option<&str> {
        match &self.mode {
            InputMode::AddKeyLabel { buf } => Some(buf.as_str()),
            _ => None,
        }
    }

    pub(crate) fn pending_oauth_disable_entry(&self) -> Option<&str> {
        match &self.mode {
            InputMode::ConfirmOAuthDisable { entry_id } => Some(entry_id.as_str()),
            _ => None,
        }
    }

    pub(crate) fn admin_busy(&self) -> bool {
        self.resources.admin.inline.is_some()
    }

    pub(crate) fn reset_screen_local_mode(&mut self) {
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
pub(crate) enum UiMsg {
    Tick,
    Key(KeyEvent),
    Admin(Box<AdminCompletion>),
    LogLine(appliance_log::ApplianceLogEntry),
}
