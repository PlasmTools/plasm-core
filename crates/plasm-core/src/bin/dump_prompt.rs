//! Emit the eval **teaching table** prompt for a CGS directory ([`PromptPipelineConfig::default`]: default **TSV** `Expression`/`Meaning` table + comment contract). For the compact markdown teaching string, set `render_mode` to **compact** on the pipeline (e.g. `plasm-mcp --symbol-tuning compact`).
//! Per entity block, teaching TSV orders **value-domain `v#` gloss**, then **`p#` gloss**, then **union constructor exemplars** (`v101{p#=…}`, …), then the synthetic **union summary** row (`vN` / `union · v101 | …`, `N` allocated after existing map/tokens). Look for ctor exemplars near that summary, not only among bare `v#` metadata rows.
//! Only links `plasm-core` (no plasm-eval / BAML).
//!
//! ```text
//! cargo build -p plasm-core --bin dump_prompt
//! RUST_LOG=plasm_core::loader=trace,plasm_core::prompt_render=trace,info \
//!   ./target/debug/dump_prompt apis/clickup 2>trace.log > /tmp/clickup_prompt.txt
//! ```
//! The schema directory must contain `domain.yaml` and `mappings.yaml` (e.g. `fixtures/schemas/overshow_tools`).
//! If you pass `…/overshow_tool` by mistake, the loader resolves it to `overshow_tools` when that sibling exists.
//! Progress lines go to **stderr** (unbuffered) so you still see them when stdout is redirected.

use plasm_core::loader::load_schema_dir;
use plasm_core::PromptPipelineConfig;
use std::env;
use std::io::Write;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new(
                    "plasm_core::loader=trace,plasm_core::prompt_render=trace,info",
                )
            }),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let path = env::args()
        .nth(1)
        .ok_or("usage: dump_prompt <schema_dir>")?;

    eprintln!("dump_prompt: loading CGS from {path} …");
    let _ = std::io::stderr().flush();
    let cgs = load_schema_dir(path.as_ref())?;
    eprintln!(
        "dump_prompt: CGS loaded ({} entities, {} capabilities); rendering prompt …",
        cgs.entities.len(),
        cgs.capabilities.len()
    );
    let _ = std::io::stderr().flush();

    let pipeline = PromptPipelineConfig::default();
    let s = pipeline.render_prompt(&cgs, None);
    let st = pipeline.prompt_surface_stats(&cgs, None, &s);
    eprintln!(
        "dump_prompt: prompt built — {}; writing stdout …",
        st.summary_line_body()
    );
    let _ = std::io::stderr().flush();

    std::io::stdout().write_all(s.as_bytes())?;
    std::io::stdout().flush()?;
    Ok(())
}
