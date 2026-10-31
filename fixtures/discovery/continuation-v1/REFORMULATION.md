# Frozen reformulation experiment — 2026-10-31

Development experiment, not a holdout. Frozen before the following native runs.

Hypothesis: sharing candidate visit counts across current-request reformulations
reduces repeated admission, but may delay a previously rejected operation when
the outcome or its constraints change. The earlier v18 scope controls already
suggest that risk; this is not an unforeseen result or a new confirmatory claim.

Factor: visit-state scope only. Arms: exact current request (production v18),
and session-wide visit counts (experimental, not a production switch).

Fixed: native connected planner and exact packet construction, original 16,000
UTF-8-byte limit, mandatory acquisitions, root allocation, authored descriptions,
recall order per scenario, no retained judgments/exposure, no provider calls.
Counts record hypothetical rejected judgments solely to exercise scheduling.
This does not test the model's relevance accuracy or semantic paraphrase detection.

Cases: independent abstract operations, with 8 and 64 distractors. A required
operation is fifth in recall for six differently worded continuations. The same
six calls with identical request text are the continuation control. Two further
controls put a previously judged operation first in current recall: changed
constraint and genuinely new outcome. Both must reach judgment immediately.
All operations fit individually; the joint catalogue exceeds the packet budget.
Fixture identities are opaque. Required-operation labels are assertions only.

Metrics: first turn reaching the required operation, unique judged candidates,
repeated candidate judgments and maximum encoded packet size. Six-turn horizon,
one packet per turn, deterministic, no model/seed/temperature applicable.

Promotion gate: reformulations reach the required operation within six turns,
same-request continuation still advances, both changed-request controls admit
their newly requested operation on the first turn, and all packets respect the
unchanged limit. Report both arms and all failures. At most these two arms in this
experiment; no post-result tuning. A failed control means no production change.

Recorded failures are separate consumed diagnostics: verify absent required
operations and repeated candidate identities from the v18 receipts. Those
receipts cannot prove a counterfactual selection or live recovery. Ranking
versus allocation replay with actual recall remains a separate experiment.
