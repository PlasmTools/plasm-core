# Return discovery design to operational outcomes

The retain-admitted diagnostic did not improve useful-path availability. Across all
58 paired frozen variants, **first completion and prefix logical-request counts
were identical**. The six trajectory starvation failures remained. All 32 breadth
and eight ordinary variants retained their previous availability result. No
production behavior was changed and no provider or AppWorld evaluation ran.

## What the whole pipeline measured

| Measure, summed across development variants | Fixture judgment | Retain admitted |
|---|---:|---:|
| Logical fixture requests through completion or horizon | 165 | 165 |
| Useful-no-gain searches in that prefix | 98 | 98 |
| Admitted packet bytes in that prefix | 2,478,526 | 2,462,229 |
| Full declaration surface at completion/horizon | 42,696 bytes | 165,228 bytes |
| Incidental exposed capabilities at completion/horizon | 0 | 1,417 |

Declaration totals are sums of endpoint surface sizes, **not cumulative transmitted
teaching bytes or model tokens**. The approximately 3.9-fold increase is evidence of
a larger available surface, not a measured agent-context penalty. Cohorts include
intentional duplicate variants; sums are descriptive, not population estimates.

The small 16,297-byte packet reduction does not change any completion or request
count. It is not provider-token evidence. Retain-admitted still constructs bounded
packets and uses local fixture responses; it is not an implementation with those
steps removed. Actual model cost, downstream agent context, and adaptive behavior
remain unmeasured. Therefore this experiment does not prove that retaining an LLM
filter is cheaper overall, or that a deterministic filter would reproduce it.

All 116 arm/variant records completed, each with reversed-insertion parity. Both
arms used one logical fixture response at most and 16,000 bytes at most per search.
All frozen cohort locks held. Current baseline first completion, prefix requests,
prefix bytes and declaration size matched all 58 surviving historical variants.
Native test execution took 300.1 seconds, excluding build. No fixture, label or
budget changed after measurement; no effect execution or semantic provider call.

The original no-unrelated-exposure criterion remains recorded and fails in the
retain-admitted arm. That is not secretly relabelled into a product pass. The new
operational comparison also fails the frozen dominance criterion: no starvation
recovery and larger declaration surfaces. Incidental exposure is not an execution
violation; it matters only through resource cost, interference or caller burden.

## Mechanical location of the remaining starvation

In the 1,000-distractor repeated membership case, `cabinet_read` remains lexical
rank 1,005 and budget-deferred through search 24 in the retain-admitted arm.
The action and optional specimen read are present, but membership is incomplete.
In the independent-outcome case, `publish` likewise remains rank 1,005 and deferred,
while firing, preparation, sample read and the opaque routine are retained.

Removing negative judgments did not repair those losses. That identifies the
remaining obstruction in this fixture as candidate ordering/admission rather than
rejection. It does not prove that the real model never rejects useful capabilities,
or that all recorded agent failures share this cause. Do not strengthen that claim.

## Design priorities across the system

1. **Useful-path coverage under bounded admission.** This has the strongest native
   gradient: low-ranked independent resources and membership access lose to large
   runs of competing operations. Reassess candidate coverage and allocation together
   before another semantic-classifier tweak. Preserve complete paths, alternatives,
   ordinary requests and opaque uncertainty. Test a single mechanism against every
   frozen row and report the cost/coverage frontier, not only an aggregate score.
2. **Caller progress and usable exposure.** Retained agent evidence includes repeated
   discovery after useful writes had already been exposed. The caller must know what
   is available and what remains unresolved; a no-new response does not establish
   global exhaustion or task completion. A separate small closed-loop test is needed
   to measure use of that feedback. Do not add automatic stopping or forced execution
   based solely on exposed capability sets.
3. **Program repair and terminal submission.** Late row-field, typed-key and plural
   extraction failures consumed the remaining step budget; another recorded failure
   had the correct answer in prose but no authoritative submission. Keep abstract
   admission and termination tests separate. Discovery cannot claim these repairs.
4. **Judgment's implementation cost.** Filtering demonstrably reduces declaration
   breadth here without delaying useful paths relative to retain-admitted. Whether
   that benefit warrants a remote model call is still an end-to-end cost question.
   Preserve a retain-admitted comparator when evaluating cheaper filters or a changed
   semantic boundary; don't optimize relevant-versus-uncertain labels as a proxy.

The prior resource-diversity arm recovered all six starvation gates but delayed a
zero-distractor independent case from one search to two. Its original rejection
stands. Nevertheless it is a real coverage/cost tradeoff, unlike the recent renderer's
zero retention change. A future experiment should declare acceptable tradeoffs in
advance rather than silently treating either strict per-case dominance or an
aggregate improvement as the universal objective. Existing labels and reports stay
unchanged. No new ranking policy is selected or shipped by this report.

## Scope and evidence

Discovery is monotonic capability acquisition: expose sufficient useful operations
promptly and cheaply so the caller can do its work. Exposure is neither execution
permission nor a complete execution plan. Existing session capabilities remain.

This suite uses scripted self-contained requests, fixture judgments and shared
abstract schemas. It does not measure live semantic vectors, adaptive agents,
execution correctness, terminal submission, or a full-task success rate. The
single-service case still does not reproduce the recorded wrong-service failure.
That coverage limitation remains visible rather than filled by a claimed fix.

Detailed evidence is in `eval-reports/retention-value-v1-20261030/`: frozen locks,
TEST.log, RESULTS.jsonl (all native receipts), AUDIT.json (all paired comparisons),
REPORT.md/REPORT.html and verified hashes. Prior system failures were reviewed from
the parent's `eval-reports/appworld-resilient-transport-test-normal-20261030/TRIAGE.md`.
