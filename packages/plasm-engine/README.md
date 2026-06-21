# @plasm_lang/engine

NAPI native addon for [`plasm-node`](../../crates/plasm-node).

## Build

```bash
npm install
npm run build
```

Produces a platform `.node` binary and generated `index.js` loader.

## API

See `index.d.ts` — mirrors `PlasmEngine` in the Rust crate:

- `loadCatalog(catalogDir)`
- `exposeSeeds(intent, seeds)`
- `dryRun(program)`

Live execute (`run` with host transport) is not wired in v0.
