//! CLI entry point for the Plasm semantic projection system.

mod commands;

use clap::{Parser, Subcommand, ValueEnum};
use plasm_runtime::ExecutionMode;

/// CLI mirror of [`ExecutionMode`] for `plasm execute --mode`.
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CliExecutionMode {
    #[default]
    Live,
    Replay,
    Hybrid,
}

impl From<CliExecutionMode> for ExecutionMode {
    fn from(m: CliExecutionMode) -> Self {
        match m {
            CliExecutionMode::Live => ExecutionMode::Live,
            CliExecutionMode::Replay => ExecutionMode::Replay,
            CliExecutionMode::Hybrid => ExecutionMode::Hybrid,
        }
    }
}

#[derive(Parser)]
#[command(
    name = "plasm-cgs",
    version = env!("CARGO_PKG_VERSION"),
    about = "A semantic projection layer for REST APIs"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Validate a CGS schema file
    Schema {
        #[command(subcommand)]
        action: SchemaAction,
    },
    /// Type-check a predicate against schema
    Predicate {
        #[command(subcommand)]
        action: PredicateAction,
    },
    /// Compile predicates to backend requests
    Compile { schema: String, predicate: String },
    /// Execute queries with full round-trip
    Execute {
        schema: String,
        predicate: String,
        #[arg(long, value_enum, default_value_t = CliExecutionMode::Live)]
        mode: CliExecutionMode,
    },
    /// Record and replay operations
    Replay {
        #[command(subcommand)]
        action: ReplayAction,
    },
    /// Emit the CGS domain model as Mermaid ER diagram text (erDiagram). Paste into mermaid.live or a Markdown ```mermaid block; older Mermaid embeds may parse fewer features.
    ErDiagram {
        /// CGS schema directory (domain.yaml + mappings.yaml) or YAML file
        schema: String,
        #[arg(short, long, value_name = "PATH")]
        output: Option<std::path::PathBuf>,
        /// Omit entity attribute blocks; relationships only
        #[arg(long)]
        relations_only: bool,
        #[arg(long, value_enum)]
        direction: Option<commands::er_diagram::ErDirection>,
    },
    /// Exhaustively validate a CGS against an OpenAPI mock (hermit in-process); exercises list pagination when declared
    #[command(alias = "verify")]
    Validate {
        /// CGS schema directory (domain.yaml + mappings.yaml) or JSON file
        schema: String,
        /// OpenAPI spec file to serve as mock backend
        #[arg(long)]
        spec: String,
        /// Reject OpenAPI operations without a catalog mapping before running Hermit
        #[arg(long)]
        require_complete: bool,
    },
}

#[derive(Subcommand)]
enum SchemaAction {
    Validate { file: String },
}

#[derive(Subcommand)]
enum PredicateAction {
    Check { schema: String, predicate: String },
}

#[derive(Subcommand)]
enum ReplayAction {
    Record { schema: String, predicate: String },
    Test { dir: String },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Schema { action } => commands::schema::execute(action).await,
        Commands::Predicate { action } => commands::predicate::execute(action).await,
        Commands::Compile { schema, predicate } => {
            commands::compile::execute(&schema, &predicate).await
        }
        Commands::Execute {
            schema,
            predicate,
            mode,
        } => commands::execute::execute(&schema, &predicate, mode.into()).await,
        Commands::Replay { action } => commands::replay::execute(action).await,
        Commands::ErDiagram {
            schema,
            output,
            relations_only,
            direction,
        } => commands::er_diagram::execute(&schema, output, relations_only, direction).await,
        Commands::Validate {
            schema,
            spec,
            require_complete,
        } => commands::validate::execute(&schema, &spec, require_complete).await,
    }
}
