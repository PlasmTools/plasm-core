# @plasm_lang/engine

NAPI native addon for [`plasm-node`](../../crates/plasm-node).

## Build

```bash
npm install
npm run build
```

Produces the native addon and the matching pinned Monty worker. Import the package
root, whose `runtime.cjs` entry point supplies the bundled worker to each engine.
Platform npm packages ship both artifacts and the worker's MIT license. The
worker is never resolved beside the Node executable.

`new PlasmEngine()` uses the packaged runtime. Deployments may explicitly supply
an absolute worker path with `new PlasmEngine('/absolute/path/to/monty')` or
`PLASM_MONTY_BINARY`; it must match the pinned `monty-pool` revision. This does not
modify the process environment. Each engine shares a bounded pool across its
logical sessions.

## API

See `index.d.ts` — mirrors `PlasmEngine` in the Rust crate:

- `loadCatalog(catalogDir)`
- `exposeSeeds(intent, seeds)`
- `dryRun(program)`

`dryRun(program)` reviews a Python `Program`; `runPlanLive(planCommitRef, transport)`
executes the reviewed DAG asynchronously using the supplied HTTP callback.
