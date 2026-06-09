//! Shared imports re-exported to TUI submodules via `pub(crate) use prelude::*` in `mod.rs`.

#![allow(unused_imports)]

pub use std::collections::{HashSet, VecDeque};
pub use std::io;
pub use std::sync::atomic::{AtomicBool, Ordering};
pub use std::sync::Arc;
pub use std::time::{Duration, Instant};

pub use crossbeam_channel::Sender;
pub use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
pub use plasm_agent_core::mcp_config_admin::{
    McpCatalogAuthMarker, McpConfigApiKeyRow, McpConfigCatalogRow,
};
pub use plasm_agent_core::server_state::PlasmHostState;
pub use ratatui::backend::CrosstermBackend;
pub use ratatui::layout::{Constraint, Direction, Layout, Rect};
pub use ratatui::style::{Color, Modifier, Style};
pub use ratatui::text::{Line, Span};
pub use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
pub use ratatui::Terminal;
pub use uuid::Uuid;

pub(crate) use crate::appliance_admin_bridge::{
    config_surface_from_host, AdminBridge, AdminCompletion, AdminCorr, AdminJob,
    McpConfigSurfaceState, OAuthSurfaceState, PolicyStoreUnavailableReason, RefreshedUiData,
};
pub(crate) use crate::appliance_log;
pub(crate) use crate::appliance_mcp_admin::appliance_mcp_scope;
pub(crate) use crate::appliance_mode::PolicyStoreBootstrapDetail;
pub(crate) use crate::boot::UiEvent;
pub(crate) use crate::oauth_upsert_wizard::{OAuthUpsertStep, OAuthUpsertWizard};
