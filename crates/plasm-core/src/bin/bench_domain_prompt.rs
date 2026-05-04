//! Benchmark DOMAIN prompt rendering for a CGS directory.
//!
//! Default schema is `apis/pokeapi` (~50 entities / ~100 capabilities in-repo — heavier than most
//! single-API bundles; try `apis/clickup` or `fixtures/schemas/pokeapi_mini` for contrasts).
//!
//! Does **not** use Criterion; prints wall times to stderr so you can profile with `samply` / `perf`:
//!
//! ```text
//! cargo build -p plasm-core --release --bin bench_domain_prompt
//! samply record target/release/bench_domain_prompt apis/pokeapi
//! ```
//!
//! From the repo root, a relative path is resolved against `crates/plasm-core/../../` (workspace root).
//!
//! Usage:
//! ```text
//! bench_domain_prompt [schema_dir] [iterations] [warmup]
//! bench_domain_prompt [iterations] [warmup]   # default schema: apis/pokeapi
//! ```

use plasm_core::loader::load_schema_dir;
use plasm_core::prompt_render::{
    render_domain_prompt_bundle, render_domain_prompt_bundle_for_exposure, RenderConfig,
};
use plasm_core::symbol_tuning::{domain_exposure_session_from_focus, FocusSpec};
use std::env;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn resolve_schema_dir(arg: &str) -> PathBuf {
    let p = PathBuf::from(arg);
    if p.is_dir() {
        return p;
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let from_root = manifest.join("../..").join(arg);
    if from_root.is_dir() {
        return from_root;
    }
    p
}

fn median_ms(samples: &mut [f64]) -> f64 {
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = samples.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        samples[n / 2]
    } else {
        (samples[n / 2 - 1] + samples[n / 2]) / 2.0
    }
}

fn bench_iter<F: FnMut()>(iters: usize, warmup: usize, mut f: F) -> Vec<f64> {
    for _ in 0..warmup {
        f();
    }
    let mut out = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t0 = Instant::now();
        f();
        out.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let default_schema = "apis/pokeapi";
    let (path_str, iters, warmup) = match args.as_slice() {
        [] => (default_schema.to_string(), 10usize, 2usize),
        [a] if a.parse::<usize>().is_ok() => {
            let it: usize = a.parse().unwrap();
            (default_schema.to_string(), it, 2usize)
        }
        [a, b] if a.parse::<usize>().is_ok() && b.parse::<usize>().is_ok() => (
            default_schema.to_string(),
            a.parse().unwrap(),
            b.parse().unwrap(),
        ),
        [path, rest @ ..] => {
            let it = rest.first().and_then(|s| s.parse().ok()).unwrap_or(10usize);
            let wu = rest.get(1).and_then(|s| s.parse().ok()).unwrap_or(2usize);
            (path.clone(), it, wu)
        }
    };

    let dir = resolve_schema_dir(&path_str);
    if !dir.is_dir() {
        return Err(format!(
            "not a directory: {} (tried {} and workspace-relative path)",
            path_str,
            dir.display()
        )
        .into());
    }

    eprintln!(
        "bench_domain_prompt: loading {} …",
        std::fs::canonicalize(&dir)
            .unwrap_or_else(|_| dir.clone())
            .display()
    );
    let t_load = Instant::now();
    let cgs = load_schema_dir(Path::new(&dir))?;
    let load_ms = t_load.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "  load_schema_dir: {:.2} ms ({} entities, {} capabilities)",
        load_ms,
        cgs.entities.len(),
        cgs.capabilities.len()
    );

    let cfg = RenderConfig::for_eval(None);

    eprintln!(
        "  benchmark: render_domain_prompt_bundle (TSV symbols, FocusSpec::All), iters={iters} warmup={warmup}"
    );
    let mut full_samples = bench_iter(iters, warmup, || {
        let b = render_domain_prompt_bundle(&cgs, cfg);
        black_box(
            b.teaching_blocks
                .iter()
                .map(|bl| bl.teaching_rows.len())
                .sum::<usize>(),
        );
    });
    let med_full = median_ms(&mut full_samples);
    eprintln!(
        "  render_domain_prompt_bundle: median {:.2} ms (min {:.2}, max {:.2})",
        med_full,
        full_samples.iter().copied().fold(f64::INFINITY, f64::min),
        full_samples.iter().copied().fold(0.0_f64, f64::max),
    );

    eprintln!("  precomputing DomainExposureSession (FocusSpec::All) …");
    let t_exp = Instant::now();
    let exposure = domain_exposure_session_from_focus(&cgs, FocusSpec::All);
    eprintln!(
        "  domain_exposure_session_from_focus: {:.2} ms ({} exposed entities)",
        t_exp.elapsed().as_secs_f64() * 1000.0,
        exposure.entities.len()
    );

    eprintln!(
        "  benchmark: render_domain_prompt_bundle_for_exposure only (reuse session), iters={iters} warmup={warmup}"
    );
    let mut exp_samples = bench_iter(iters, warmup, || {
        let b = render_domain_prompt_bundle_for_exposure(&cgs, cfg, &exposure, None);
        black_box(
            b.teaching_blocks
                .iter()
                .map(|bl| bl.teaching_rows.len())
                .sum::<usize>(),
        );
    });
    let med_exp = median_ms(&mut exp_samples);
    eprintln!(
        "  render_domain_prompt_bundle_for_exposure: median {:.2} ms (min {:.2}, max {:.2})",
        med_exp,
        exp_samples.iter().copied().fold(f64::INFINITY, f64::min),
        exp_samples.iter().copied().fold(0.0_f64, f64::max),
    );
    eprintln!(
        "  delta (full includes fresh DomainExposureSession per iter): {:.2} ms",
        med_full - med_exp
    );

    eprintln!("done.");
    Ok(())
}
