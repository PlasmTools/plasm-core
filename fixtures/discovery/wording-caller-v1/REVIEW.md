# Review of the exploratory wording experiment

The completed 2026-11-01 run is identified by its original source LOCK.json in
`eval-reports/wording-caller-v1-20261101`. Original runner sources are preserved
there under `original-source/`. Review changes below were made after that run;
they do not retroactively change its preregistration, raw rows or source hashes.

Review controls now reject schema-invalid tool calls before state mutation,
retain interrupted trajectories and their accumulated generations separately
from unstarted work, and distinguish completion without the required observation
from a terminal value that fails the exact-answer judge after observation.

Historical results: baseline 6/10 and candidate 7/10 passed the strict judge.
Discovery calls were 20 versus 21; post-exposure searches 4 versus 2. All seven
strict failures were answer-format mismatches after the required observation,
not unsupported completion claims. The original premature-completion aggregate
therefore mislabels these failures. The manual audit records that limitation.
No provider faults occurred. Raw generations remain the evidence for usage.

This small mock-environment study does not establish a discovery-quality win or
justify production promotion. No provider rerun was performed during review.
Candidate rendering in the offline native-wrapper check and this mock is separate:
the latter has a synthetic schedule and repeats declarations, so neither is a
byte-for-byte parity claim about the full production tool response.

Offline controls:

```sh
node --import ../scripts/appworld/agent/node_modules/tsx/dist/loader.mjs scripts/discovery-experiments/wording-caller-v1/test.mjs
```
