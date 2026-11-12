# @plasm_lang/engine

NAPI native addon for [`plasm-node`](../../crates/plasm-node).

## Build

```bash
npm install
npm run build
```

Initialize sources with `git submodule update --init --recursive` first.
Produces the native addon and the matching worker from `vendor/monty`. Import the package
root, whose `runtime.cjs` entry point supplies the bundled worker to each engine.
Platform npm packages ship both artifacts and the worker's MIT license. The
worker is never resolved beside the Node executable.

`new PlasmEngine()` uses the packaged runtime. Deployments may explicitly supply
an absolute worker path with `new PlasmEngine('/absolute/path/to/monty')` or
`PLASM_MONTY_BINARY`; it must match the vendored `monty-pool` source. This does not
modify the process environment. Each engine shares a bounded pool across its
logical sessions.

## API

See `index.d.ts` — mirrors `PlasmEngine` in the Rust crate:

- `loadCatalog(catalogDir)`
- `exposeSeeds(intent, seeds)`
- `dryRun(program)`

`dryRun(program)` reviews a Python `Program`; `runPlanLive(planCommitRef, transport)`
executes the reviewed DAG asynchronously using the supplied HTTP callback.

## Workflow teaching archives

`plasm-workflow-archive` is a host administration command, not an agent tool.
It exports scored whole-program observations and teaching libraries through the
OSS repository. Import validates source provenance and current catalogue
compatibility before existing portfolio rules can admit examples. It never
executes the archived program or installs its receipts as local executions.

The configuration contains a target scope and an environment-variable name,
not a credential:

```json
{"scope":{"tenant":"training","project":"corpus"},"database_env":"WORKFLOW_CORPUS_URL"}
```

Export requests identify `kind` (`private_collection` or `serving_library`) and
`collection`, containing a 64-character `protocol_digest` and the registered
`tasks` array of `{ "task_id": "...", "group_id": "..." }` objects. Optional
assessment input is a host-generated array; task grades are not assertions of
individual program success.

```sh
plasm-workflow-archive export --config scope.json --request export-request.json \
  --assessments task-assessments.json --output training.archive.json \
  --audit export-audit.jsonl
```

Private collection archives can contain private runtime evidence. Serving-library
archives exclude it and explicitly reference supporting evidence that must be
retained at the destination before adoption. Transfer the private archive only
within the intended authorized boundary; a checksum is integrity, not trust.

An import policy names `expected_digest`, the original `source_scope`, and
`excluded_tasks` / `excluded_groups` for the consuming evaluation. The destination
scope comes from configuration, independently of the immutable source scope.
Catalogue input is a JSON array of authoritative packed manifest paths; bindings
are the destination's deployment bindings. Discovery storage uses the normal
`PLASM_DISCOVERY_DATABASE_URL` configuration.

```sh
plasm-workflow-archive import --config destination-scope.json \
  --archive training.archive.json --policy import-policy.json \
  --catalogs catalog-manifests.json --bindings deployment-bindings.json \
  --deployment evaluation-library --output import-report.json \
  --audit import-audit.jsonl
```

Output and audit files must be new and are created with private permissions.
Imports are idempotent by archive identity. Retain the source archive unchanged
and use independent destination libraries for evaluation arms. Evaluation
sessions must not promote their own outcomes into the consumed library.

For an exact round-trip of an imported archive, use `export --digest DIGEST`
instead of `--request`; this returns the retained original document and preserves
its source scope and item identities. It cannot attach new assessments.

After import, verify ordinary serving against the destination's activated catalogue
and deployment bindings without executing any example:

```sh
plasm-workflow-archive verify --config destination-scope.json \
  --archive training.archive.json --catalogs catalogs.json --bindings bindings.json \
  --deployment verification --output compatibility.json --audit compatibility-audit.json
```

The host checks at most 32 eligible revisions, each in a fresh context, through
normal symbol binding and Python admission. The private report binds the retained
archive, catalogue files and deployment bindings by SHA-256 and records typed
per-revision outcomes. Empty, truncated or partially unsuccessful reports fail the
command while retaining the report. This proves serving compatibility only; it
neither executes effects nor measures semantic recall or task usefulness.
