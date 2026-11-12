# Continuation and response interface: exploratory development diagnostic

Frozen before measurement, 2026-11-01. No model calls, embeddings, dependency
changes, deployed policy changes or AppWorld rerun. Existing unrelated WIP remains.

## Mechanical track

Hypothesis: request-scoped visit identity loses exploration progress under
reformulation; session-scoped visits can improve coverage but delay reconsideration
after a genuine change. Compare those two scopes only. Use production frontier
identity, pinned-CGS recall validation, allocation, packet rendering, response
decoding and byte limits. Recall is explicitly frozen, not real hybrid retrieval.
Persistence is an in-memory map, not a PostgreSQL/concurrency proof.

Six turns, four neutral read operations; same request versus six paraphrases;
unchanged intent versus a newly useful formerly rejected operation after turn three;
fitting versus individually oversized target. All 16 cells are development data.
No holdout or agent-success claim. The changed-intent labels are a diagnostic oracle,
not a semantic interpreter. No labels enter recall, ordering or packet construction.

Report every cell: target judgment by turn, unique candidates, repeated judgments,
admitted candidates, bytes and defer reasons. Useful target missing by turn four is
a quality failure, except oversized cells where non-admission is the integrity
control. In changed-intent cells the first target is evaluated before the change
(turns one through three), and the new target must be judged on turn four.
Changed intent must permit a new positive judgment; never reuse negative
judgments as an exclusion. No post-hoc horizon or packet expansion. Stop after one
baseline comparison; report null and adverse results. Native test success measures
integrity only, not quality. This track deliberately does not retain exposure, to
isolate exploration scheduling from exposure suppression.

## Response wording track — separate factor

The current caller receives a bounded-search banner plus initial/additional/no-new
declaration wording, or a no-relevant-selection branch. Inspect these actual
renderers before designing candidates. Keep CGS teaching verbatim.

Freeze neutral states: newly exposed capability; already available capability;
no relevant judgment in this packet with unexplored candidates; byte-deferred
candidate; no progress on another request; changed intent requiring reconsideration.
Audit which distinctions the production response can actually express. A response
must never turn partial search into catalogue absence, exposure into execution,
or available declarations into task completion. Do not claim that the server knows
whether the caller's task is solved. Do not recommend another identical call when
there is no recorded reason to expect progress.

Proposed wording is a candidate interface, not deployed teaching. A later caller
experiment must hold capability packets, judgments, model and budgets fixed and
vary only wording; measure unnecessary rediscovery, correct execution, premature
stopping and false claims of absence. No caller-model run is part of this diagnostic.

Alternative composition, cross-domain identities and collection semantics remain
separate required coverage: this scheduling experiment does not establish them.

## Commands

From the core root, run the native `frozen_continuation_diagnostic` unit test with
`--nocapture` through `build/bazel/linux.sh`; pass its captured stdout to
`python3 fixtures/discovery/continuation-scope-v2/summarize.py LOG`.
From `packages/plasm-agent`, run
`node_modules/.bin/tsx scripts/diagnose-discovery-wording.ts`.
The summarizer prints quality failures separately; a zero native test exit is not
a quality pass. No experiment arm is a production configuration option.
