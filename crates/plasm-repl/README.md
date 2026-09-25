# Python DAG REPL

`cargo run -p plasm-repl --features baml -- --schema fixtures/schemas/python_dag_slice --backend http://localhost:1080`

The REPL prints the Python library reference and catalog-derived domain declarations.
Paste one `class Example(Program)` with `build(self)`, preserving indentation, then
enter `:plan` to review or `:run` to execute. `:load path.py` loads a whole source file.
`:show` displays the buffer; `:reset` discards it. A failed compile retains source for repair.

The outer class declares a DAG. Deferred formatting uses typed `@compute` methods in
bounded Monty workers. Calls, effects, result types and correction diagnostics use
the same compiler as HTTP/MCP. Native Plasm source is not accepted.

`:schema Entity` extends declarations without changing existing symbols; `:schema`
prints the accumulated reference. `:clear` clears materialized rows while retaining
symbols. `:mode live|replay|hybrid` and `:output json|table|compact` control execution
and results. TSV remains a result format, not teaching syntax.

`:llm MODEL` enables OpenRouter/BAML goal translation (`OPENROUTER_API_KEY` required).
`:llm attempts N` sets bounded correction attempts; `:llm off` returns to source input.
Generated Python remains buffered for explicit `:plan` / `:run`. The initial reference
is supplied once; domain extensions and corrections retain the same logical context.

Generate the pinned BAML client with `baml-cli generate --from baml_src` from the OSS
root using the generator version declared in `baml_src/generators.baml`.
