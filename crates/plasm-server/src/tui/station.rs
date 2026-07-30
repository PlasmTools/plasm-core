//! RUN mode event loop

use super::*;

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
        clipboard: ClipboardService::new(),
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
