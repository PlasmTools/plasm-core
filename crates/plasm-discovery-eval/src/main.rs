use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Score frozen receipts from the shared PostgreSQL discovery service")]
struct Args {
    #[arg(long)]
    receipts: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cases: Vec<plasm_discovery_eval::FrozenCase> =
        serde_json::from_slice(&std::fs::read(args.receipts)?)?;
    let scores = cases
        .into_iter()
        .map(plasm_discovery_eval::score)
        .collect::<anyhow::Result<Vec<_>>>()?;
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(args.output)?;
    serde_json::to_writer_pretty(output, &scores)?;
    Ok(())
}
