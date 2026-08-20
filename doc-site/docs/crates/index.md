# OSS crates

Workspace layout for **[plasm-core](https://github.com/PlasmTools/plasm-core)**. Crate names link to **docs.rs** when available.

| Crate | Role |
|-------|------|
| [**plasm-core**](https://docs.rs/plasm-core) | CGS, AST, typecheck, discovery, teaching table rendering — **catalog-agnostic**. |
| [**plasm-cml**](https://docs.rs/plasm-cml) | CML AST and transport parsing (shared with compile). |
| [**plasm-compile**](https://docs.rs/plasm-compile) | Predicates, decoding, template validation. |
| [**plasm-runtime**](https://docs.rs/plasm-runtime) | Execution engine, cache, replay, auth resolution; Polars-backed [`row_compute`](https://github.com/PlasmTools/plasm-core/tree/main/crates/plasm-runtime/src/row_compute) for fused [`RowPlan`](https://github.com/PlasmTools/plasm-core/tree/main/crates/plasm-core/src/row_plan) chains. |
| [**plasm-otel**](https://github.com/PlasmTools/plasm-core/tree/main/crates/plasm-otel) | OTLP trace/metric/log bootstrap; stable `plasm_agent.*` / `plasm_core.*` / `plasm_runtime.*` span names — see [crate README](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-otel/README.md#semantic-span-names-stable-contract). |
| [**plasm-agent-core**](https://docs.rs/plasm-agent-core) | MCP host, sessions, traces, MCP sqlx metadata, HTTP execute. |
| [**plasm-server**](https://github.com/PlasmTools/plasm-core/tree/main/crates/plasm-server) | **OSS appliance** binary — in-process kernel + TUI. |
| [**plasm**](https://docs.rs/plasm) | Remote terminal **`plasm`**, **`plasm-cgs`**, **`plasm-pack-catalogs`**. |
| **plasm-repl**, **plasm-cli**, **plasm-eval**, **plasm-e2e**, **plasm-mock** | Tooling and test harnesses. |

**Dependency direction (simplified):** `plasm-core` ← `plasm-compile` ← `plasm-runtime` ← `plasm-agent-core` ← `plasm-server` / `plasm`.

Release binaries:

| Binary | Role |
|--------|------|
| **`plasm-server`** | Appliance — HTTP/MCP, control station, embedded Postgres |
| **`plasm`** | Remote HTTP terminal client |
| **`plasm-cgs`** | Schema-driven one-shot CLI (dev) |
| **`plasm-repl`** | Interactive path expressions (dev) |
| **`plasm-pack-catalogs`** | Pack `apis/<name>/` to JSON IL + manifest artifacts |

Operator docs: [Appliance quick start](../appliance/quickstart.md). Optional **features** — see each crate’s `Cargo.toml` on GitHub.
