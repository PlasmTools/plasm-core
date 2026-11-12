//! Runs tab

use super::super::*;

pub(crate) fn render(
    frame: &mut ratatui::Frame<'_>,
    layout_body: ratatui::layout::Rect,
    model: &mut RunState,
    host_state: &PlasmHostState,
    _listen: &plasm_agent_core::listen_endpoint::TcpListenEndpoint,
) {
    let (content_area, notice_area) = split_main_notice_area(layout_body, model.notice.is_some());
    let mut lines = vec![
        Line::from("Runs / traces"),
        Line::from(""),
        Line::from("Operational drill-down binds to execute session store and trace hub."),
        Line::from(if host_state.workflow_learning.enabled() { "Teaching library enabled. Every session is ordinary until explicitly authorized." } else { "Teaching library unavailable. Ordinary discovery and execution remain available." }),
        Line::from("Training capture status belongs to each logical session; host capability does not opt in."),
        Line::from("t: enter a pending consent/publication request UUID and copy its review link."),
        Line::from("Open the link in your browser, authenticate as project owner, and inspect the exact revision."),
        Line::from("This station does not grant consent or publish examples by displaying them."),
    ];
    lines.push(Line::from("l: select logical session · f: authenticated refresh · x: stop capture and hold unpublished material"));
    lines.push(Line::from("a: authorize the selected session controller for a new training epoch; held material stays held."));
    if let Some(snapshot) = &model.training {
        lines.push(Line::from(format!(
            "Selected session: {}",
            plasm_agent_core::mcp_logical_ref::format_logical_session_wire_ref(snapshot.logical)
        )));
        lines.push(Line::from(format!(
            "Revision {} · {:?} · {:?} · held: {}",
            snapshot.status.revision,
            snapshot.status.training_state,
            snapshot.status.capture_availability,
            snapshot.status.held
        )));
    }
    match &model.mode {
        InputMode::TrainingSelect { logical_ref } => {
            lines.push(Line::from(format!(
                "Logical session reference: {logical_ref}_"
            )));
            lines.push(Line::from(
                "Enter selects; authenticated owner proof is required before disclosure.",
            ));
        }
        InputMode::TrainingOwner { action, secret, .. } => {
            lines.push(Line::from(match action { TrainingAction::Status => "Authenticate to read this session's training status.", TrainingAction::Stop { .. } => "Authenticate to STOP capture and HOLD unpublished material. Published examples remain available.", TrainingAction::AuthorizeController { .. } => "Authenticate to grant this session's controller capture and review for 24 hours. Publication requires separate approval." }));
            lines.push(Line::from(format!(
                "Owner setup credential: {}",
                "•".repeat(secret.character_count().min(64))
            )));
            lines.push(Line::from("Enter submits this exact action. Esc cancels. Credential is not retained in the UI."));
        }
        _ => {}
    }
    if let InputMode::TrainingReview { request_ref } = &model.mode {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("Review request UUID: {request_ref}_")));
        lines.push(Line::from(
            "Enter copies the authenticated review link. Esc cancels.",
        ));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(chrome::panel_block("Runs", Some('r'))),
        content_area,
    );
    if let (Some(area), Some(notice)) = (notice_area, model.notice.as_ref()) {
        render_notice_panel(frame, area, notice);
    }
}

/// Only trusted process configuration selects the approval host.
pub(crate) fn training_review_url(reference: Uuid) -> Option<String> {
    let base = std::env::var("PLASM_TRAINING_APPROVAL_BASE_URL").ok()?;
    let base = base.trim();
    if !(base.starts_with("https://") || base.starts_with("http://"))
        || base.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(format!(
        "{}/training/requests/{reference}",
        base.trim_end_matches('/')
    ))
}
