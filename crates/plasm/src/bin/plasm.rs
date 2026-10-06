//! Remote HTTP terminal (discovery + execute sessions) for a Plasm server.

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match plasm_agent::run_cgs_main().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
