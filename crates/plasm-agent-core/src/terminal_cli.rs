//! Strongly typed Clap surface for the remote `plasm` HTTP terminal.

use anyhow::{bail, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

use crate::resolved_plan_http::ResolvedPlanRunMode;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum RunModeCli {
    /// Compile/validate only (no live side effects).
    Plan,
    /// Live execution (default).
    Run,
}

impl From<RunModeCli> for ResolvedPlanRunMode {
    fn from(m: RunModeCli) -> Self {
        match m {
            RunModeCli::Plan => ResolvedPlanRunMode::Plan,
            RunModeCli::Run => ResolvedPlanRunMode::Run,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum AcceptMediaCli {
    /// Table-oriented text (`text/plain`).
    #[value(name = "plain")]
    Plain,
    #[value(name = "toon")]
    Toon,
    #[value(name = "json")]
    Json,
    #[value(name = "ndjson")]
    Ndjson,
}

impl AcceptMediaCli {
    pub fn as_accept_header(self) -> &'static str {
        match self {
            Self::Plain => "text/plain",
            Self::Toon => "text/toon",
            Self::Json => "application/json",
            Self::Ndjson => "application/x-ndjson",
        }
    }
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[arg(
        long,
        short = 'm',
        value_enum,
        default_value_t = RunModeCli::Run,
        help = "Plan-only dry compile vs live run"
    )]
    pub mode: RunModeCli,

    #[arg(
        long,
        value_enum,
        default_value_t = AcceptMediaCli::Plain,
        help = "Result media type for execute response"
    )]
    pub accept: AcceptMediaCli,

    #[arg(
        long,
        short = 'f',
        value_name = "PATH",
        help = "Plasm program file; read stdin when omitted"
    )]
    pub file: Option<PathBuf>,

    #[arg(
        long,
        default_value_t = true,
        help = "When false, start live run in background and return wait(sN_oM) immediately"
    )]
    pub wait: bool,

    #[arg(long, help = "Bypass dry-run review soft gate on live execute")]
    pub force: bool,

    #[arg(
        long,
        value_name = "PCN",
        help = "Plan commit ref (pcN) from a prior plan dry-run"
    )]
    pub plan_commit_ref: Option<String>,
}

#[derive(Debug, Args)]
pub struct ContextArgs {
    #[arg(long, help = "New client session (fresh teaching.tsv)")]
    pub new: bool,

    #[arg(long, help = "Print full TSV exposure block")]
    pub verbose: bool,

    #[arg(
        long,
        short = 'i',
        value_name = "TEXT",
        help = "Agent intent (required with --new; else defaults to last `plasm search` intent)"
    )]
    pub intent: Option<String>,

    #[arg(
        value_name = "CATALOG:ENTITY",
        required = true,
        num_args = 1..,
        help = "Registry entry_id:entity (e.g. pokeapi:Pokemon); required format with --new"
    )]
    pub seeds: Vec<String>,
}

/// Returns true when `seed` is a non-empty `entry_id:Entity` pair.
pub fn is_qualified_seed(seed: &str) -> bool {
    let seed = seed.trim();
    match seed.split_once(':') {
        Some((api, ent)) => !api.trim().is_empty() && !ent.trim().is_empty(),
        None => false,
    }
}

/// Validate context CLI args after Clap parse (`--new` rules).
pub fn validate_context_args(args: &ContextArgs) -> Result<()> {
    if args.new {
        if args
            .intent
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .is_none()
        {
            bail!("context --new requires --intent (-i)");
        }
        for seed in &args.seeds {
            if !is_qualified_seed(seed) {
                bail!(
                    "context --new requires catalog:entity seeds (e.g. pokeapi:Pokemon), not `{seed}`"
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug, Parser)]
#[command(
    name = "plasm",
    version = env!("CARGO_PKG_VERSION"),
    about = "Remote Plasm terminal — search, context, run (HTTP). Run `plasm init` once, then `doctor` if needed.",
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        default_value = "default",
        help = "Profile name under .plasm/profiles/ in the workspace (cwd)"
    )]
    pub profile: String,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    #[command(about = "Configure local profile and optional platform sign-in")]
    Init {
        #[arg(
            long,
            value_name = "URL",
            help = "HTTP API origin (e.g. http://127.0.0.1:3000)"
        )]
        server: Option<String>,
        #[arg(long, help = "API key for local/appliance hosts")]
        api_key: Option<String>,
        #[arg(
            long,
            help = "Skip GitHub device login when --server is a managed platform origin"
        )]
        no_login: bool,
    },
    #[command(about = "Sign in to a managed platform host via device OAuth")]
    Login,
    #[command(about = "Profile, auth, and GET /v1/health diagnostics")]
    Doctor,
    #[command(about = "Discover capabilities; merge into hosts/<slug>/discovery.tsv")]
    Search {
        #[arg(
            value_name = "INTENT",
            help = "Natural-language goal for capability discovery"
        )]
        intent: String,
        #[arg(long, help = "Maximum ranked candidates to return")]
        limit: Option<usize>,
    },
    #[command(
        about = "Expose entities into the client symbol space",
        long_about = "Append teaching rows to teaching.tsv for the active client session. \
                      Use registry entry_id:Entity seeds (e.g. pokeapi:Pokemon). \
                      With --new, --intent and qualified seeds are required. \
                      Without --new, unqualified entity names may resolve via `plasm search` cache when unique."
    )]
    Context {
        #[command(flatten)]
        context: ContextArgs,
    },
    #[command(
        about = "Run or plan an expanded Plasm program",
        long_about = "Requires an active context (`plasm context`). Expands local e#/p# symbols, \
                      POSTs resolved plan JSON to the server. Use --mode plan for dry compile only."
    )]
    Run {
        #[command(flatten)]
        run: RunArgs,
    },
    #[command(about = "Verify hash-chained evidence bundle JSON")]
    Evidence {
        #[command(subcommand)]
        cmd: EvidenceCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum EvidenceCmd {
    #[command(about = "Verify chain integrity (and optional run seal)")]
    Verify {
        #[arg(value_name = "FILE", help = "Path to .evidence.json sidecar")]
        path: std::path::PathBuf,
        #[arg(
            long,
            help = "Verify run_sealed digest for this run_id (requires --artifact)"
        )]
        run_id: Option<String>,
        #[arg(long, help = "Run snapshot JSON for run_sealed digest verification")]
        artifact: Option<std::path::PathBuf>,
        #[arg(
            long,
            help = "CGS schema directory to parse artifact expressions (required with --artifact for digest verify)"
        )]
        schema: Option<std::path::PathBuf>,
        #[arg(
            long = "trusted-pubkey",
            help = "Allowed Ed25519 public key (hex); repeat for rotation window (also PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS)"
        )]
        trusted_pubkey: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_command_debug_assert() {
        Cli::command().debug_assert();
    }

    #[test]
    fn run_rejects_unknown_mode() {
        assert!(Cli::try_parse_from(["plasm", "run", "--mode", "execute"]).is_err());
    }

    #[test]
    fn run_rejects_unknown_accept() {
        assert!(Cli::try_parse_from(["plasm", "run", "--accept", "soap"]).is_err());
    }

    #[test]
    fn run_accept_json_maps_header() {
        let cli = Cli::try_parse_from(["plasm", "run", "--accept", "json"]).expect("parse");
        let Cmd::Run { run } = cli.cmd else {
            panic!("expected run");
        };
        assert_eq!(run.accept.as_accept_header(), "application/json");
    }

    #[test]
    fn context_new_requires_qualified_seeds() {
        let args = ContextArgs {
            new: true,
            verbose: false,
            intent: Some("x".into()),
            seeds: vec!["Pokemon".into()],
        };
        let err = validate_context_args(&args).unwrap_err();
        assert!(err.to_string().contains("catalog:entity"));
    }

    #[test]
    fn is_qualified_seed_accepts_entry_entity() {
        assert!(is_qualified_seed("pokeapi:Pokemon"));
        assert!(!is_qualified_seed("Pokemon"));
        assert!(!is_qualified_seed(":Pokemon"));
    }
}
