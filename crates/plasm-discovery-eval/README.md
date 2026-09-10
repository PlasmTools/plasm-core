# Compiled discovery receipt evaluation

The evaluator scores frozen JSON receipts emitted by the shared PostgreSQL discovery
service. It contains no discovery backend, alternative ranker or selector prompt.

`plasm-discovery-eval --receipts frozen-cases.json --output scores.json`

Each case supplies its ID, exact expected business and prerequisite capability
references, allowed capabilities and full routing receipt. The output reports missing
and excess capabilities, unauthorized exposure and false Ready per case. Output files
are created exclusively to preserve frozen results. This deterministic accounting is
not a substitute for preregistered held-out task completion and paraphrase studies.
