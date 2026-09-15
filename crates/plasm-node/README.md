# plasm-node

Node.js NAPI bindings for the catalog-native Plasm agent engine. The native surface loads packed catalog manifests, exposes teaching, compiles and reviews plans, and executes committed plans through a host transport callback. The generated `packages/plasm-engine/index.d.ts` defines the current API.

## Build

From `packages/plasm-engine`, with workspace dependencies installed:

```bash
npm run build        # release
npm run build:debug  # debug
```

Or compile the Rust crate only with `cargo build -p plasm-node` (add `--release` for optimized code).

## Debug execution stack regression

Execution must work on a 2 MiB worker stack in both profiles. Heap boundaries at context commits and plan I/O dispatch keep child futures out of their callers' frames; execution task-local scopes wrap one boxed future without intermediate async blocks.

From the workspace root:

```bash
cargo test -p plasm-agent-core --lib stack_budget
cargo test -p plasm-e2e --test semantic_receiver
cargo test -p plasm-runtime --lib credential_create_returns_receipt_and_cached_reads_revalidate_scope
cargo test -p plasm-node --lib --features napi/dyn-symbols live_
```

The native fanout test compiles and executes an abstract matrix workflow on an explicitly sized 2 MiB thread and verifies both writes through an in-memory transport. `napi/dyn-symbols` lets standalone Rust tests link without a Node host; it does not replace the execution engine or increase its stack. The transport-invariance test uses a loopback server.

## Rust diagnostics

The NAPI `PlasmEngine` constructor initializes a process-wide Rust tracing subscriber
once, writing to stderr (never the agent stdout protocol). If an embedder has already
installed a subscriber, it retains ownership. Default level: `warn`. Set `RUST_LOG`
before creating the first engine; the filter is fixed at initialization.

To select hydration boundary diagnostics:

```sh
RUST_LOG=warn,plasm_runtime::hydration=trace node your-agent.js
```

Each hydration attempt has a correlation ID and compiled request fingerprint. Events record dispatch, HTTP status and
body size, returned JSON shape, decode input/output, merge disposition, and final
workspace row shape. The start event includes a BLAKE3 digest of the requested Ref,
not the raw identity. Shapes include field names, types, string byte lengths and array
lengths, so missing, null and empty fields remain distinct. Diagnostic events omit
headers, scalar payload values and error messages. These events describe the runtime
hydration boundary; compare the final row to the stored artifact when investigating
later publication loss. Replay results identify their source and need not have a live
HTTP response event. This switch does not change retries or execution behavior.
