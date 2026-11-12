# Native discovery trajectory gradient

This supplements the existing fixed, starvation and progression suites. It measures the combined cost of repeated discovery needs, competing services, connected paths and actual incremental teaching. It does not run an agent or call a model.

Inputs are locked in `LOCK.json`; do not refreeze them after seeing results. See `PROTOCOL.md` for exclusions and frozen gates. The deliberate rejection diagnostic is never included in quality totals.

From the `plasm-oss` root:

```sh
python3 fixtures/discovery/trajectory-v4/run.py verify
python3 fixtures/discovery/trajectory-v4/test_report.py
python3 fixtures/discovery/trajectory-v4/run.py run eval-reports/trajectory-candidate
python3 fixtures/discovery/trajectory-v4/run.py quality eval-reports/trajectory-candidate/RESULTS.json
python3 fixtures/discovery/trajectory-v4/compare.py eval-reports/discovery-trajectory-v4-baseline-20261030-r2 eval-reports/trajectory-candidate
```

`run` uses the supported Linux Bazel build and real native selector, packet renderer and incremental teaching surface. It records implementation hashes before testing and checks them afterward. Each output directory must be new. Native harness failures are distinct from quality failures; `quality` exits nonzero for any incomplete path at search four, even if it becomes complete later.

Use the report's per-case first completion, missing-outcome searches and no-gain prefix as the gradient. Inspect `RESULTS.json` for exact packets, rank/frontier/defer/decision stages and relation visibility. A green case means its authored path became teachable; it does not mean a generated program executed or a final answer was submitted.

The same fixed needs are exercised in repeat and paraphrase-cycle modes. Neither is described as an adaptive agent. A provider judgment track, host persistence replay, and minimal program/terminal protocol tests are separate validation layers; do not merge their scores with this mechanical suite.
