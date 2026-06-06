# plasm-eval

Deterministic and LLM-backed evaluation harnesses over Plasm schemas and case files.

## Design boundary: no domain leakage

Plasm is a **general-purpose language and runtime for API mapping** (schema, expressions, CML, execution). **Domain-specific knowledge is forbidden in this crate:** no branches on particular CGS entity or capability names from `apis/…`, no field-alias or env-key hacks for one vendor’s HTTP templates, and no special transport cases tied to a single product.

Catalog behavior belongs in **`apis/<name>/`**, fixtures, and optional **plugins**—expressed as data and schema-driven rules. Code here stays **agnostic**, driven only by loaded CGS and generic IR/types.

**LLM eval (`plasm-eval` default run):** all cases execute **in YAML order** on **one BAML `TranslatePlan` transcript** (teaching table/schema only in the first user turn; each case appends a `--- GOAL ---` turn, mirroring `plasm-repl` `:llm`). There is no parallel “job” mode.

See [AGENTS.md](../../AGENTS.md) for workspace layout and commands.
