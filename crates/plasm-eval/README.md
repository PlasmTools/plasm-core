# Python DAG evaluator

`plasm-eval` translates goals to complete Python `Program` classes and compiles them
with the same session, type checks, dry planning and structured corrections as HTTP/MCP.
It never repairs source by retrying the retired native parser. Correction feedback
preserves indentation and string contents; repeated rejection evidence is session-local.

```sh
cargo run -p plasm-eval --features baml -- --schema fixtures/schemas/python_dag_slice --print-prompt
cargo run -p plasm-eval --features baml -- --schema apis/example --cases apis/example/eval/cases.yaml --model MODEL
```

`--focus Entity` selects initial declarations. The first transcript turn supplies
one Python library reference and domain declarations. Later turns reuse that context.
Semantic scoring inspects compiled DAG operations, row filters, projections, relation
traversals, effects and nested bodies. Source-text expectations inspect original Python.

`reference_expr` in case YAML now contains a complete Python Program, with the symbol
allocation of this evaluator session. Old native references must be migrated; they
are rejected, not silently rewritten. Coverage metadata (`covers`, `expect`) remains
catalog-oriented. A deterministic coverage report is not an LLM benchmark.

`--print-prompt` prints Python teaching. Retired `--print-prompt-tsv` and
`--symbol-tuning` authoring options are not supported. TSV remains a result encoding.
Generate BAML with the pinned version declared by `baml_src/generators.baml`.
