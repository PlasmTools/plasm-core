//! Interactive Python DAG REPL (`plasm-eval` / BAML for `:llm` mode).

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    plasm_repl::run_repl_main().await
}
