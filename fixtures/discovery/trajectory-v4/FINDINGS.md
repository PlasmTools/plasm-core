# Frozen baseline: a mechanical starvation gradient now exists

Baseline evidence: `eval-reports/discovery-trajectory-v4-baseline-20261030-r2/`. Fixtures were locked before measurement. The earlier invocation was a harness-format failure preserved separately; see ERRATA.md. No requests, labels or catalogues were changed after freezing.

The native integrity test passed, while the separate quality command exited **1** with **6/18 failing trajectory variants**. Eighteen means three structural cases × three catalogue sizes × two request schedules, not eighteen independent scenarios. Every variant was also run with reversed capability insertion order; semantic traces were identical. The explicit rejection diagnostic was excluded from the quality total.

| Family | 0 distractors | 100 distractors | 1,000 distractors |
|---|---|---|---|
| Competing service effects | Complete at search 1 | Complete at search 1 | Complete at search 1 |
| Collection members, repeated request | Complete at search 1 | Complete at search 10; 8 no-gain calls | Incomplete after search 24; 23 no-gain calls |
| Collection members, rotating requests | Complete at search 1 | Complete at search 2 | Complete at search 2 |
| Independent outcomes, either schedule | Complete at search 1 | Complete at search 11; 9 no-gain calls | Incomplete after search 24; 23 no-gain calls |

## What is actually failing

In the repeated collection case, `stamp` and the optional specimen read are retained early. The necessary cabinet read has lexical rank 105 at 100 distractors and 1,005 at 1,000 distractors. At search four it is explicitly budget-deferred. At 1,000 distractors it remains deferred at search 24, after 258 journal candidates have reached judgment. Capability recall for the action alone would hide the failure; the full membership path does not exist.

In the independent-outcomes case, firing and the opaque routine are available at search one, but publication is not. Publication has lexical rank 105/1,005 and remains budget-deferred while unrelated journal candidates consume the frontier. At 1,000 distractors, neither repeated nor rotating requests recovers the second outcome within 24 searches.

These are combined ranking/allocation losses visible before judgment. The fixture judge never rejects the useful missing targets. Do not claim allocation alone caused the low rank, or classifier quality caused the deferral. Exact ranks, frontier positions, defer receipts, packets and decisions are preserved per turn in RESULTS.json.

The paired request schedules matter: paraphrasing recovers collection membership but does not recover the independent outcome. Improving only a favourable query is therefore insufficient. The single-service-effect path passes all sizes; this fixture has NOT reproduced the recorded wrong-service failure. That remains a coverage limitation, not an excluded failure or a claimed fix.

## Integrity and cost

- One logical fixture response maximum per search; maximum observed packet exactly 16,000 UTF-8 bytes.
- Native target contract checks, authorization checks, monotonic exposure and insertion-order parity passed.
- Actual teaching relation visibility participates in the collection completeness gate.
- Provider calls and agent evaluations: zero. Provider tokens/cost/accuracy: unmeasured.
- Native test execution for the entire matrix and diagnostic: 97.8 seconds, excluding build/setup. No production ranking or policy was changed.
- Quality reporting tests prove late completion fails the gate and post-completion cost does not inflate the prefix-to-completion metric.

## Use as a design gradient

Keep this baseline and lock unchanged. Compare any later implementation with `compare.py`, reporting every row: first complete search, per-outcome completion, missing-outcome searches, no-gain calls and request/byte deltas. Recovering the delayed member read must not starve publication, lose the opaque routine, or regress the currently passing service path. Do not increase the packet budget or hide quality failures behind native test success.

This is a mechanical selector-and-teaching development test. Scripted follow-ups and fixture judgments do not measure adaptive agent behavior, host persistence, semantic vectors, execution correctness or terminal submission. Its red result provides a cheap local gradient for two observed failure mechanisms; a future green result is not permission to declare the entire agent viable.
