# Fixed native discovery suite

This is a hash-locked **development regression benchmark**, not a blind holdout.
It executes native projection, authorization, ranking, connected allocation,
packet construction and response decoding, with fixture judgments replacing only
the provider callback. It never invokes an execution backend or AppWorld.

From the monorepo:

```sh
python3 plasm-oss/fixtures/discovery/fixed-v1/run.py verify
python3 plasm-oss/fixtures/discovery/fixed-v1/run.py run /absolute/new/report-directory
python3 plasm-oss/fixtures/discovery/fixed-v1/run.py summary /absolute/report-directory/RESULTS.json
```

From the OSS checkout, omit `plasm-oss/`. Output directories must be new. The
runner preserves logs on failure, pins native sources for the duration of a run,
and refuses changed frozen inputs. Do not regenerate `cases.json` or update
`LOCK.json` to accommodate a ranking change; create an explicitly new version if
the fixture specification itself is wrong.

`RESULTS.json` preserves full packets and per-search receipts. `SUMMARY.json`
separates useful/uncertain target dispositions and complete-path coverage at
searches 1, 2 and 4. Quality failures are data, not assertion failures. Native
invariant or harness failures remain hard failures. `MANIFEST.json` and
`SHA256.json` identify sources, fixtures and outputs.

Recorded AppWorld failures are excluded from this matrix. Provider token usage,
cost and judgment accuracy are unavailable. The search horizon does not alter
production budgets. See `PROTOCOL.md` for scope and the parent repository's
`docs/discovery-fixed-evaluation.md` for the wider acceptance-suite design.
