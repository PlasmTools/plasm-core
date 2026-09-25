# Remote Plasm terminal (`plasm`)

`plasm` is a transport-only HTTP client for `plasm-server` or `plasm-mcp`.
The server owns the pinned catalog generation, append-only symbols, Python
teaching, compilation and reviewed execution. For local schema work, use
`plasm-repl --schema …` or the `plasm-cgs` schema tooling.

## Setup and context

```bash
plasm init --server http://127.0.0.1:3000 --api-key "$PLASM_API_KEY"
plasm doctor
plasm search "inspect pokemon combat data"
plasm context --new -i "inspect pokemon combat data"
```

Context uses the server's intent routing. Read its returned library reference and
typed declarations before authoring a program. Extend the same context with
`plasm context -i "additional work"`; use `--new` for a separate workflow.
The terminal does not allocate or rewrite entity/method symbols locally.

## Plan and execute Python

Write a complete `class …(Program)` with `build(self)` and an explicit return in
`program.py`, using only the declarations exposed in this context. Then:

```bash
plasm run --mode plan --file program.py
plasm run --file program.py --plan-commit-ref pc0
```

Replace `pc0` with the returned review reference. The terminal sends the source
unchanged to the pinned execute session. `--file` is optional; otherwise source
comes from stdin. Preserve Python indentation and multiline literal contents.
Use `--accept plain`, `json` or `ndjson` for results; plain is the CLI default.
Compilation errors return the production Python corrections. Repair within the
same session; do not start a fresh context to recover from a syntax error.

## Local evidence

Profiles live under `.plasm/profiles/`; the active host pointer selects the local
session mirror. Context operations under `.plasm/s/<id>/out/NNNN-context/`
contain `routing.json` and `teaching.md`. Teaching is Markdown containing Python
library/declaration blocks, not a TSV grammar table. TSV remains a result format.
Run operations mirror the submitted program, response and linked artifacts.
`PLASM_WORKSPACE` overrides the project directory used for local state.

For hosted sign-in use `plasm init --server https://platform.plasm.tools/plasm/http`
and `plasm login`. Use `plasm evidence verify --help` for evidence verification.

See [Python language definition](plasm-language-definition.md) and
[incremental teaching](incremental-teaching-prompts.md).
