# plasm-node

Node.js NAPI bindings for the catalog-native Plasm agent engine.

Exposes catalog load, agent-global teaching exposure, parse/compile, and plan dry-run (`pcN`) from `plasm-core` + `plasm-agent-core` to TypeScript via `@plasm_lang/engine`.

## Build

From `packages/plasm-engine`:

```bash
npm install
npm run build
```

Or compile the Rust crate only:

```bash
cargo build -p plasm-node --release
```

## Surface (v0)

- `loadCatalog(catalogDir)` — load `domain.yaml` + `mappings.yaml`, pin `catalog_cgs_hash`
- `exposeSeeds(intent, seeds)` — append to agent-global `TeachingExposureSession`, return teaching TSV delta
- `dryRun(program)` — compile + dry-run, mint `run_ref` (`pcN`)

Live execute with host transport callback is not wired in v0.
