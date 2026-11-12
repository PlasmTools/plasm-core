# Frozen discovery experiment harnesses

These preserve development experiment source, not evaluation results or a deployed
objective-transition contract. Registrations, cases and prompts live in
`fixtures/discovery/caller-loop-v1` and `caller-revision-v1`.
Run only with explicit authorization for provider calls. Neither review nor offline
preflight requires a live model.

Both runners require `PLASM_EXPERIMENT_OUTPUT` pointing at a fresh evidence directory.
They require the parent monorepo's existing `scripts/appworld/agent` installation and
its TypeScript loader (`node --import tsx`, with tsx resolved from that installation).
No dependency installation is performed. They retain the original Luna settings,
exclusive LAUNCHED marker and source-hash checks.

## Closed loop

1. Run `caller-loop-v1/run.mjs --freeze` to copy cases and lock source hashes.
2. Start the ignored native `frozen_caller_loop_worker` test with
   `PLASM_CALLER_LOOP_ROOT` set to that same directory (use its container-visible
   path for Linux Bazel). Run with `--ignored --nocapture`.
3. Run `caller-loop-v1/preflight.mjs` for the mocked-fetch SDK-history proof.
4. When READY.json exists, run `run.mjs --preflight`; only the separately
   authorized `--run` mode invokes the provider.
5. Run `python3 caller-loop-v1/audit.py` with the same output environment.

The worker uses numbered request/response JSON files, ends on STOP, and does not
run retrieval, relevance providers, or effects. The caller sends STOP on exit.
The worker/runner use the same directory; do not reuse a finished directory.
Audit stdout is the result; save it outside tracked source. Request fidelity
remains a separate manual check specified by the registration.

## Single-transition smoke

The `caller-revision-v1` runner additionally requires `PLASM_NATIVE_SUMMARY`, an
explicit path to the native objective-revision experiment's SUMMARY.json. It is
locked before provider calls. This artifact is not bundled: projections are
lookups against those observations, not fresh native executions. Run `--freeze`,
then `--preflight`, and only with authorization `--run`. The Python judge uses
the same two environment variables.

These extracted harnesses have parameterized paths and include their judge in
the source lock. Historical result hashes apply to the preserved originals, not
these extracted versions; no historical result is relabelled as a new run.
