//! Remote training controls; all consent and admission remain server-owned.
use super::*;
use crate::terminal_cli::{TrainingCmd, TrainingMutationArgs};

fn session_path(session: &str, suffix: &str) -> Result<String> {
    crate::mcp_logical_ref::parse_logical_session_wire_ref(session)
        .map_err(TerminalError::TrainingSession)?;
    Ok(format!("/v1/training/sessions/{session}{suffix}"))
}

fn mutation(
    args: TrainingMutationArgs,
    action: &str,
) -> Result<(Method, String, Option<serde_json::Value>)> {
    Ok((
        Method::POST,
        session_path(&args.session, "/commands")?,
        Some(
            serde_json::json!({"action":action,"expected_revision":args.expected_revision,"idempotency_key":args.idempotency_key}),
        ),
    ))
}

fn read_json(path: PathBuf) -> Result<serde_json::Value> {
    let bytes =
        std::fs::read(&path).map_err(|source| TerminalError::ProgramFileRead { path, source })?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn request(cmd: TrainingCmd) -> Result<(Method, String, Option<serde_json::Value>)> {
    match cmd {
        TrainingCmd::Status { session } => Ok((Method::GET, session_path(&session, "")?, None)),
        TrainingCmd::Request(args) => mutation(args, "request"),
        TrainingCmd::Stop(args) => mutation(args, "stop"),
        TrainingCmd::Resume(args) => mutation(args, "resume"),
        TrainingCmd::Review { session, file } => Ok((
            Method::POST,
            session_path(&session, "/reviews")?,
            Some(read_json(file)?),
        )),
        TrainingCmd::Candidates { session, limit } => Ok((
            Method::GET,
            format!("{}?limit={limit}", session_path(&session, "/candidates")?),
            None,
        )),
        TrainingCmd::Decide { file } => Ok((
            Method::POST,
            "/v1/training/requests".into(),
            Some(read_json(file)?),
        )),
        TrainingCmd::ReleaseRuns { file } => Ok((
            Method::POST,
            "/v1/training/run-releases".into(),
            Some(read_json(file)?),
        )),
        TrainingCmd::Release { file } => Ok((
            Method::POST,
            "/v1/training/releases".into(),
            Some(read_json(file)?),
        )),
    }
}

pub(super) async fn run(profile: &TerminalProfile, cmd: TrainingCmd) -> Result<()> {
    let (method, path, payload) = request(cmd)?;
    let server = require_configured_server(profile)?;
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(TerminalError::HttpClient)?;
    let (status, _, bytes) = send_bytes(
        &client,
        &server,
        profile,
        method,
        &path,
        Some("application/json"),
        Some("application/json"),
        payload
            .map(|value| serde_json::to_vec(&value))
            .transpose()?,
    )
    .await?;
    if !status.is_success() {
        return Err(TerminalError::TrainingRejected {
            status,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    std::io::stdout().write_all(&bytes)?;
    println!();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_cli::Cli;

    #[test]
    fn training_mutations_require_explicit_revision_and_idempotency() {
        assert!(
            Cli::try_parse_from(["plasm", "training", "request", "--session", "test"]).is_err()
        );
        let session = crate::mcp_logical_ref::format_logical_session_wire_ref(
            crate::session_identity::LogicalSessionId::new_v4(),
        );
        let id = uuid::Uuid::new_v4();
        let (method, path, body) = mutation(
            TrainingMutationArgs {
                session: session.clone(),
                expected_revision: 3,
                idempotency_key: id,
            },
            "request",
        )
        .unwrap();
        assert_eq!(method, Method::POST);
        assert_eq!(path, format!("/v1/training/sessions/{session}/commands"));
        assert_eq!(body.unwrap()["idempotency_key"], id.to_string());
        assert!(session_path("../../decisions", "").is_err());
    }
}
