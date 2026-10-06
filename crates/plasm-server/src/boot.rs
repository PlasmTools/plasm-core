//! Bootstrap checklist TUI: alternate-screen startup phases, then handoff to [`crate::tui::run_running_mode`].

use std::io::{self, stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use plasm_agent_core::listen_endpoint::TcpListenEndpoint;
use plasm_agent_core::server_state::PlasmHostState;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::appliance_admin_bridge::AdminBridge;
use crate::appliance_mode::PolicyStoreBootstrapDetail;
use crate::tui::run_running_mode;

pub const BOOT_PHASE_COUNT: usize = 8;

#[derive(Debug, thiserror::Error)]
pub enum TerminalUiError {
    #[error("terminal UI I/O failed")]
    Io(#[from] io::Error),
    #[error("RUN terminal UI failed")]
    Running(#[source] io::Error),
}

/// Handoff from async bootstrap to the Ratatui UI thread (host state + admin job bridge).
pub struct RunningHandoff {
    pub state: Arc<PlasmHostState>,
    pub admin_bridge: AdminBridge,
    /// When the policy store did not attach (migrate failed or no URL).
    pub policy_store_detail: Option<PolicyStoreBootstrapDetail>,
}

/// Rolling BOOT Detail log capacity (FIFO trim).
pub const BOOT_DETAIL_MAX_LINES: usize = 12;

pub static BOOT_PHASE_LABELS: [&str; BOOT_PHASE_COUNT] = [
    "Load catalog",
    "Validate templates",
    "Embedded PostgreSQL",
    "Build engine + host state",
    "Attach OSS extensions",
    "Bind HTTP listener",
    "Start MCP listener",
    "Control station ready",
];

/// UI thread → async supervisor (cross-thread).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiEvent {
    /// RUN loop has started drawing at least once.
    RunEntered,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum RowState {
    #[default]
    Pending,
    Active,
    Done,
    Skipped,
}

struct BootModel {
    rows: [RowState; BOOT_PHASE_COUNT],
    skip_reason: [Option<String>; BOOT_PHASE_COUNT],
    /// Multi-line status log (`DetailPush`) or replaced summary (`Detail`).
    detail_lines: Vec<String>,
    fatal: Option<String>,
    started: Instant,
}

impl BootModel {
    fn new() -> Self {
        Self {
            rows: [RowState::Pending; BOOT_PHASE_COUNT],
            skip_reason: Default::default(),
            detail_lines: Vec::new(),
            fatal: None,
            started: Instant::now(),
        }
    }

    fn apply(&mut self, msg: BootstrapUiMsg) {
        match msg {
            BootstrapUiMsg::PhaseEnter(i) => {
                if i < BOOT_PHASE_COUNT {
                    self.rows[i] = RowState::Active;
                    self.skip_reason[i] = None;
                }
                // Fresh Detail for load catalog and later phases; keep lines during phase 1
                // so the consolidated catalog summary remains visible.
                if i == 0 || i >= 2 {
                    self.detail_lines.clear();
                }
            }
            BootstrapUiMsg::PhaseDone(i) => {
                if i < BOOT_PHASE_COUNT {
                    self.rows[i] = RowState::Done;
                    self.skip_reason[i] = None;
                }
            }
            BootstrapUiMsg::PhaseSkip(i, reason) => {
                if i < BOOT_PHASE_COUNT {
                    self.rows[i] = RowState::Skipped;
                    self.skip_reason[i] = Some(reason);
                }
            }
            BootstrapUiMsg::DetailPush(line) => {
                self.detail_lines.push(line);
                while self.detail_lines.len() > BOOT_DETAIL_MAX_LINES {
                    self.detail_lines.remove(0);
                }
            }
            BootstrapUiMsg::Detail(d) => {
                self.detail_lines = if d.trim().is_empty() {
                    Vec::new()
                } else {
                    d.lines().map(std::string::ToString::to_string).collect()
                };
            }
            BootstrapUiMsg::Fatal(s) => self.fatal = Some(s),
            BootstrapUiMsg::Running(_) | BootstrapUiMsg::Shutdown => {}
        }
    }
}

pub enum BootstrapUiMsg {
    PhaseEnter(usize),
    PhaseDone(usize),
    PhaseSkip(usize, String),
    /// Append one status line; trims oldest rows past [`BOOT_DETAIL_MAX_LINES`].
    DetailPush(String),
    /// Replace the Detail buffer (e.g. final catalog summary).
    Detail(String),
    Fatal(String),
    Running(RunningHandoff),
    Shutdown,
}

fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some()
}

fn accent_style() -> Style {
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

fn marker_for(row: RowState) -> &'static str {
    match row {
        RowState::Pending => "[ ]",
        RowState::Active => "[~]",
        RowState::Done => "[x]",
        RowState::Skipped => "[-]",
    }
}

fn startup_lines(model: &BootModel) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::with_capacity(BOOT_PHASE_COUNT);
    for (i, (&row, &label)) in model.rows.iter().zip(BOOT_PHASE_LABELS.iter()).enumerate() {
        let m = marker_for(row);
        let mark_style = match row {
            RowState::Active => accent_style(),
            RowState::Pending => dim_style(),
            RowState::Done | RowState::Skipped => Style::default().add_modifier(Modifier::BOLD),
        };
        let mut spans = vec![
            Span::styled(m, mark_style),
            Span::raw(" "),
            Span::raw(label.to_string()),
        ];
        if let Some(ref r) = model.skip_reason[i] {
            spans.push(Span::styled(format!(" — {r}"), dim_style()));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn draw_boot_frame(frame: &mut Frame<'_>, model: &BootModel, listen: &TcpListenEndpoint) {
    frame.render_widget(Clear, frame.area());
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(6),
            Constraint::Min(6),
            Constraint::Length(2),
        ])
        .split(frame.area());

    let elapsed = model.started.elapsed().as_secs();
    let boot_token = Span::styled("[BOOT]", accent_style());
    let header_line = Line::from(vec![
        boot_token,
        Span::raw(" PLASM APPLIANCE — startup"),
        Span::raw(format!(
            "   {}s   listen {}  (HTTP + MCP /mcp)",
            elapsed,
            listen.display_addr()
        )),
    ]);
    frame.render_widget(
        Paragraph::new(header_line).block(Block::default().borders(Borders::BOTTOM)),
        chunks[0],
    );

    if let Some(ref fatal) = model.fatal {
        let fatal_split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
            .split(chunks[1]);
        frame.render_widget(
            Paragraph::new(startup_lines(model))
                .block(Block::default().borders(Borders::ALL).title("Startup")),
            fatal_split[0],
        );

        let mut fatal_lines = vec![
            Line::from(Span::styled(
                "Startup halted. The appliance has not launched.",
                accent_style(),
            )),
            Line::from(Span::styled(
                "Review the failure below. Bootstrap faults now name the file, path, or env var that must be repaired.",
                dim_style(),
            )),
            Line::from(""),
        ];
        fatal_lines.extend(fatal.lines().map(Line::from));
        frame.render_widget(
            Paragraph::new(fatal_lines).wrap(Wrap { trim: true }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Failure Report"),
            ),
            fatal_split[1],
        );
    } else {
        frame.render_widget(
            Paragraph::new(startup_lines(model))
                .block(Block::default().borders(Borders::ALL).title("Startup")),
            chunks[1],
        );
    }

    let detail_block = Block::default().borders(Borders::ALL).title("Detail");
    let detail_para = if model.detail_lines.is_empty() {
        Paragraph::new(Line::from("…")).style(dim_style())
    } else {
        let lines: Vec<Line> = model
            .detail_lines
            .iter()
            .map(|s| Line::from(s.as_str()))
            .collect();
        Paragraph::new(lines).wrap(Wrap { trim: true })
    };
    frame.render_widget(detail_para.block(detail_block), chunks[2]);

    let footer_spans = if model.fatal.is_some() {
        vec![
            Span::styled("q", dim_style()),
            Span::raw(" exit   "),
            Span::styled("^C", dim_style()),
            Span::raw(" exit   "),
            Span::styled("Detail", dim_style()),
            Span::raw(" keeps the last loader trace"),
        ]
    } else {
        vec![
            Span::styled("q", dim_style()),
            Span::raw(" cancel startup   "),
            Span::styled("^C", dim_style()),
            Span::raw(" shutdown"),
        ]
    };
    let footer =
        Paragraph::new(Line::from(footer_spans)).block(Block::default().borders(Borders::TOP));
    frame.render_widget(footer, chunks[3]);
}

#[allow(clippy::too_many_arguments)]
pub fn run_appliance_shell(
    rx: Receiver<BootstrapUiMsg>,
    running: Arc<AtomicBool>,
    boot_cancel: Arc<AtomicBool>,
    ui_evt_tx: Option<Sender<UiEvent>>,
    listen: TcpListenEndpoint,
    log_rx: Option<crossbeam_channel::Receiver<crate::appliance_log::ApplianceLogEntry>>,
) -> Result<(), TerminalUiError> {
    enable_raw_mode()?;
    let mut buffer = stdout();
    execute!(buffer, EnterAlternateScreen)?;
    crate::stderr_log::set_alternate_screen_active(true);
    let backend = CrosstermBackend::new(buffer);
    let mut terminal = Terminal::new(backend)?;

    let restore_terminal = || {
        crate::stderr_log::set_alternate_screen_active(false);
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    };
    let _guard = scopeguard::guard((), |_| restore_terminal());

    let inner = (|| -> Result<(), TerminalUiError> {
        let mut model = BootModel::new();
        let mut dirty = true;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(BootstrapUiMsg::Running(handoff)) => {
                    tracing::info!(
                        target: "plasm_appliance_boot",
                        "UI received RUN handoff (entering RUN mode)"
                    );
                    terminal.clear()?;
                    run_running_mode(
                        &mut terminal,
                        handoff.state,
                        Arc::clone(&running),
                        ui_evt_tx.clone(),
                        listen,
                        Some(handoff.admin_bridge),
                        handoff.policy_store_detail,
                        log_rx,
                    )
                    .map_err(TerminalUiError::Running)?;
                    return Ok(());
                }
                Ok(BootstrapUiMsg::Shutdown) => return Ok(()),
                Ok(other) => {
                    model.apply(other);
                    dirty = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    crate::stderr_log::line(
                        "plasm-server: warning: bootstrap UI channel closed before RUN handoff",
                    );
                    return Ok(());
                }
            }

            if boot_cancel.load(Ordering::SeqCst) {
                return Ok(());
            }

            while event::poll(Duration::from_millis(0))? {
                match event::read()? {
                    Event::Resize(w, h) => {
                        terminal.resize(ratatui::layout::Rect::new(0, 0, w, h))?;
                        dirty = true;
                    }
                    Event::Key(key) => {
                        let raw_quit = matches!(key.code, KeyCode::Char('\x03'))
                            || (key.modifiers.contains(KeyModifiers::CONTROL)
                                && matches!(key.code, KeyCode::Char('c' | 'C')));
                        if raw_quit {
                            boot_cancel.store(true, Ordering::SeqCst);
                            if model.fatal.is_some() {
                                return Ok(());
                            }
                        } else if let KeyCode::Char('q') = key.code {
                            boot_cancel.store(true, Ordering::SeqCst);
                            if model.fatal.is_some() {
                                return Ok(());
                            }
                        }
                    }
                    _ => {}
                }
            }

            if dirty {
                terminal.draw(|f| draw_boot_frame(f, &model, &listen))?;
                dirty = false;
            }
        }
    })();

    drop(_guard);
    let _ = terminal.show_cursor();
    inner
}
