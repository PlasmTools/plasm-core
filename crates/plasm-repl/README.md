# plasm-repl

Interactive REPL for exploring expressions against a live or mock backend.

## Design boundary: no domain leakage

Plasm is a **general-purpose language and runtime for API mapping** (schema, expressions, CML, execution). **Domain-specific knowledge is forbidden in this crate:** no branches on particular CGS entity or capability names from `apis/…`, no field-alias or env-key hacks for one vendor’s HTTP templates, and no special transport cases tied to a single product.

Catalog behavior belongs in **`apis/<name>/`**, fixtures, and optional **plugins**—expressed as data and schema-driven rules. Code here stays **agnostic**, driven only by loaded CGS and generic IR/types.

The REPL is BAML-backed and therefore opt-in: generate the `plasm-eval`
client, then run `cargo run -p plasm-repl --features baml -- …`.

See [AGENTS.md](../../AGENTS.md) for workspace layout and commands.
