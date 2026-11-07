# Unified allocation v1: preregistered development experiment

Status: protocol only. No candidate implementation or measurement has run.

## Hypothesis and single factor

Admitting complete typed acquisition alternatives at the same seam as direct roots,
before reserving space for a second root, reduces searches to a usable surface
without delaying ordinary requests. The factor is packet allocation only. Existing
root ranking, session visit priority, connected-consumer priority, graph evidence,
judgment, retention, teaching, transport and limits remain fixed.

This deliberately tests the allocation policy without inventing a semantic utility
model. Ranking is an imperfect usefulness proxy, not probability or outcome coverage.
The experiment may fail to rescue low-ranked independent roots. Such a null result
is valuable attribution and does not authorize an additional heuristic in this arm.

## Exact candidate A1

Replace the two-root initialization and separate optional-expansion/root-fill loop
with one root-associated proposal loop. Keep the existing root_order output unchanged.

1. Maintain a selected bundle and exact packet-size cache local to this search.
   Preserve the existing exposed anchors among the first two root-order positions.
   Anchors consume neither a new judgment nor a new-root position.
2. Each unexposed root in root_order offers its direct question and the alternatives
   returned by existing typed acquisition variant construction for that root. Mandatory
   acquisition is always included by existing closure. Optional supplier alternatives
   use existing authored input bindings and the existing 64-variant enumeration bound.
   Do not introduce an additional catalogue cutoff or enumerate arbitrary graph walks.
3. Reuse the same existing one-hop nomination rules from proposed roots and anchors
   for non-input optional evidence (including possible creators). Do not reinterpret
   membership as an input dependency. Associate such a proposal with the originating
   root position; ties between origins use the earlier position. This preserves their
   eligibility rather than deleting that part of the current search space.
4. Deduplicate equivalent proposals by exact capability/route identity. Evaluate only
   proposals adding at least one unexposed question. Compute incremental bytes as the
   exact serialized union size minus the current packet size; shared definitions are
   charged once. Reject only a proposal that cannot fit, not the capability globally.
5. Select the lexicographically smallest fitting tuple:
   (origin root position, completion class, incremental bytes, added question count,
   canonical proposal identity). Completion class 0 means an alternative covering all
   currently projected input-supplier groups for that root, class 1 its direct question,
   class 2 other optional nominations. When no supplier group is projected, direct
   and complete are the same proposal in class 0. Required-only acquisitions do not
   create optional supplier groups.
6. Add the entire winning proposal atomically, recompute affected proposals and repeat
   until no proposal fits. Never truncate a contract. Finite new-question additions
   guarantee termination; proposal generation remains bounded by existing alternatives.
7. Keep direct proposals eligible even when every supplier alternative is too large,
   cyclic, unavailable or absent. Known-input and opaque actions must remain judgeable.
   A complete typed proposal is only composition evidence, not known applicability,
   semantic task completion or permission. Each target is judged independently.

Origin rank dominates this policy; no novelty bonus, entity quota, label-informed
utility, query decomposition, new classifier or semantic similarity channel is added.
The current mixture of short-path, served-slot and root-fill priorities is removed
from this candidate allocation loop, not retained as a second product mode.
Alternative proposals for already satisfied roots must not be repeatedly added solely
to complete every possible supplier route: after a root's complete proposal is
selected, its other class-0 alternatives are no longer completion demands. Direct
proposals and independently nominated roots remain eligible on their own merits.

## Fixed conditions

- Baseline: current v13 allocation, including the restored original root_order.
- One logical fixture judgment/search, 16,000 UTF-8 bytes, existing retry limits
  unchanged (fixture responses do not retry), 24-search horizon, four-search gate.
- Complete contracts, exact scopes, current-request-only meaning, audit ancestry,
  monotonic exposure, uncertainty retention and authorization unchanged.
- No task-specific names, vocabulary rules, execution effects, new dependencies,
  embeddings, provider calls, AppWorld run, prompt changes or model changes.
- Deterministic fixture judge, no sampling seed or temperature; native model name is
  carried in the envelope but no model is contacted. Reverse insertion parity required.

## Cohorts and evidence

Measure every variant in all three frozen development cohorts:

- trajectory-v4: 18 variants, three structural families; six baseline gate failures.
- resource-breadth-v5: 32 variants, four structural families; all baseline gates pass,
  distributed repeated membership finishes at the four-search boundary.
- root-diversity-v1: eight ordinary variants, four structural cases; all finish at one.

These are 58 variants, not 58 independent domains or a sealed acceptance set. They
share schema and consumed cases. Do not combine their scores into a generalization
headline. Existing fixture locks, baseline summary hashes and source identity are
recorded in LOCK.json. No new labels or catalogue changes belong to this experiment.

Original v4 semantic baseline is verified by trajectory-v4-harness-parity-20261030.
Report comparisons against that parity run, the v5 baseline-r2 and the ordinary
root-diversity baseline. Preserve all raw requests, targets, ranks, deferrals,
decisions, teaching outcomes and timings. The deliberately rejected v4 judgment
diagnostic remains separate from quality scoring.

## Judge, success and stop rule

Adoption requires ALL of the following:

1. Recover at least one of the six v4 four-search failures; no previously passing
   variant fails its gate.
2. No variant that completed in baseline takes more searches or prefix logical
   requests; ordinary cases must remain one-search. An incomplete baseline is not
   assigned an invented completion time.
3. Sum of prefix admitted bytes decreases in v4. Report every per-case byte increase
   in every cohort, even if the total falls. Never trade a budget increase for success.
4. Authorization, full contracts, independent outcomes, opaque uncertainty, alternative
   supplier eligibility, monotonic teaching and insertion parity hold. No visit or
   completed-work reset. All seven service ownership/continuation proofs still pass.
5. Record construction timing separately from provider latency (unavailable). Compare
   prefix work and forced post-completion work separately. A performance concern blocks
   an efficiency claim until measured under matched conditions; single runs are
   diagnostic rather than a speedup estimate.

One candidate only. Run the complete matrix even when an early quality case fails;
abort on harness integrity faults, preserving evidence. A harness repair must be
documented and cannot change fixture semantics. Do not alter priorities, budgets,
labels, exclusions or acceptance criteria after observing candidate results. If any
adoption criterion fails, restore production allocation and retain the null result.
Do not automatically test a second candidate. Future arms require a new protocol.

Before any adopted behavior is used with durable state, choose a fresh policy identity
and update native and TypeScript contracts together. The rejected v14 identity must
not be reused for a different algorithm. Candidate code and manifests must identify
the experimental policy even if it is later rejected. No commit or deployment is
authorized by this protocol.

## Confounds and falsifiers

The main risk is substituting structural completeness for usefulness: a caller may
already have inputs, and a typed supplier may be irrelevant. Another risk is spending
the packet on the best-ranked path while a separate outcome remains untouched.
Membership is a distinct relation need, so input-complete bundles may not repair it.
Alternative enumeration and repeated exact-size trials may increase local cost.
The frozen ordinary, independent, membership and opaque controls are therefore
coequal gates, not exceptions. No approximation theorem is claimed.

## Contamination check

No holdout is used or claimed. Fixture labels and gold sets stay at the judge/scorer
seam; they are unavailable to candidate generation or allocation. The algorithm uses
existing catalogue relationships and root positions only. No task program, target
name or generated outcome branch is passed to the selector. Current intent and full
contracts remain unchanged. All arms, including null results, will be reported.
