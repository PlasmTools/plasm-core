# Incremental Python teaching

Plasm serves one Python library reference followed by incremental typed domain declarations. Teaching is a `pyi` declaration block with compact CGS comments. TSV remains a result-row format; it is not a language-teaching format.

The canonical language is [Python Plasm DAG](plasm-language-definition.md). Agents submit one class derived from `Program`, with `build(self)` and an explicit return. The outer class declares a reviewed DAG. Materialized computation runs separately in bounded Monty.

## Session contract

Open a logical session with `plasm_context`. Reuse its `logical_session_ref`; extend that session when more entities or capabilities are needed. Do not reopen it to repair source.

`TeachingExposureSession` allocates append-only, session-local symbols:

- `eN`: an entity qualified by its owning registry entry.
- `mN`: a capability on that entity.
- `rN`: a declared relation.
- `vN`: a typed value-domain alias, preserving constraints and reference ownership.

Field and argument names retain their catalog wire spellings. Identical entity names in different catalogs have different symbols. There is no merged catalog graph or vendor namespace in the program.

`prepare_python_teaching_wave` compares the current exposure with `PythonTeachingState`. The first wave includes the library. Later waves include only new or changed declarations; unchanged types are not repeated. Delivery state advances only with the prepared wave. A language change requires a new language-pinned session.

## Capability completeness

A seeded entity exposes its declared reads and mutators. Related entities expose the selected read family; extending the session can expose their mutators. The renderer records a signature or an explicit unavailable reason for every selected capability. Catalog validation checks Python declaration coverage and receiver obtainability. It does not require an example in another source language.

Input and output types preserve primitives, semantic value domains, enum choices, nested objects and arrays. Entity references carry catalog ownership; materialized objects do not grant receiver or effect authority.

## Planning and correction

Submit Python source through `plasm` with the existing logical session. The response either completes an eligible read-only plan or returns a reviewed `run_ref`. Call `plasm_run` with that ref and the same logical session; do not resend the program.

Parse, type and plan errors return structured diagnostics and an actionable Python correction. Preserve selection criteria when repairing. A failed or cancelled live run may have completed writes; inspect operation evidence before issuing more effects.

The plan renderer displays the parent composition and its data/effect dependencies. Runtime occurrence events describe the concrete work under those reviewed nodes.

## Results and artifacts

TSV encodes row previews. Session tokens use the distinct `plasm-session` fence. Large values and complete run evidence remain available through linked run artifacts. Follow the artifact URI before declaring a field absent. Tool-only clients use the advertised artifact-read tool.

Federation, persistence and stale-binding recovery retain the same logical symbol ledger. When a response explicitly signals a new symbol space, discard cached symbols and use the new declarations. See [MCP session reuse](mcp-session-reuse.md) and [client conformance](mcp-client-conformance.md).
