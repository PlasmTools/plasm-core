# Fixed discovery progress v3

Additive development regression block for the native fixed suite. Do not replace
fixed-v1 or starvation-v2.1 with it. No AppWorld catalogs or provider calls.

```sh
python3 fixtures/discovery/progress-v3/run.py verify
python3 fixtures/discovery/progress-v3/run.py run /absolute/new/output
python3 fixtures/discovery/progress-v3/run.py quality /absolute/new/output/RESULTS.json
python3 fixtures/discovery/progress-v3/compare.py /baseline/output /candidate/output
```

`run` fails on integrity/harness errors but records quality failures without hiding
them. `quality` exits nonzero for those failures. Never interpret a passing Rust
test harness as full discovery quality. Read `SUMMARY.json` and complete packets
in `RESULTS.json`. Provider tokens/cost/latency are unmeasured.

`PROTOCOL.md` and `LOCK.json` freeze expectations and fixtures before measurement.
Do not regenerate the lock to make a design pass. A correction requires a named
erratum/version preserving earlier results. Every design experiment should name
one mechanism, compare all cases, preserve negative/opaque/authorization controls,
and run the previous fixed/growth suites as guardrails.

Relationship schedules are explicitly supplied boundary diagnostics. They do not
claim the ranker found the right capabilities. Selection cases use native ranking,
allocation and fixture judgments; the teaching surface is advanced by each actual
selected delta. This exposes retained-but-untaught paths separately from allocation.

The service's SQL ownership/cache/concurrency boundary is not mocked as proven by
this suite. Continue running its six existing PostgreSQL tests independently.
Cross-domain independent acceptance remains a separate, currently unsealed task.

The fixed twelve-search horizon continues after some cases succeed. Report work
to first completion separately; later repeats are continued-search stress, not
necessary workflow overhead. A complete standalone packet above the limit is
infeasible under that representation and must not be attributed to a competing
candidate. The initial baseline contains this distinction explicitly.
