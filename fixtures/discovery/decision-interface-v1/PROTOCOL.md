# Decision-interface v1: frozen semantic diagnostic

One unchanged-model diagnostic, not a policy candidate. 24 matched pairs, four per
family, 48 packet bodies. Primary target op_a is scored once per body; the eight
auxiliary op_b questions in scope-isolation controls are recorded but not scored.
Cases, complete test catalogues, labels, generation script and this protocol are
frozen before native export. Exact native-exported bodies and sources are frozen
before provider execution. No label or rationale enters a provider request.

Use current contribution rubric, model openai/gpt-6-luna-decisions, native packet
projection/packing and decoder. Current request only; no semantic ancestry. Targets
are intentionally fixed for diagnosis: this measures neither retrieval recall nor
allocation success. Full target declarations remain, with existing projected local
witnesses. Native preparation verifies every body before any provider call.

Families: action/context, effect direction/role, readiness, local witness sufficiency,
constraints/conditional branches, question-local peer isolation. Within a pair change
only the recorded semantic factor. Cases are consumed development data, not holdouts
or 48 independent domains. Four nouns in action/context are a lexical replication,
not four separate structural families.

Labels distinguish locally admissible judgments from workflow usefulness. For the
four missing-witness controls the admissible set is uncertain/unrelated: the current
rubric does not unambiguously resolve missing evidence, and the hidden workflow is
not evidence for a positive answer. These controls are reported separately, never
used to inflate strict accuracy. Opaque isolation targets require uncertain. All
other primary labels are singletons. Any label disagreement is visible in the report.
Gold workflow usefulness for the opaque isolation pair is unknown, not false.

Run one pass, sequentially, in cases.json order, without quality retries or alternate
prompts. Maximum 48 logical requests, <=16000 UTF-8 bytes each. Reuse the unchanged
native DecisionRetryPolicy default: <=3 transient transport attempts, 30-second
whole-call budget, 8-second attempt timeout including admission. Maximum admitted
bytes 2,304,000; starts do not prove bytes transmitted. Malformed responses fail at
the native decoder, not a retry. Abort on the first unrecovered transport or harness
fault, retaining completed, failed and unstarted dispositions. Never resume a partial
run silently. Never execute catalogue effects or create embeddings.

Before live calls require the native export and existing non-ignored matcher suite
to pass, including required-ancestry supplier and cycle-deferral regressions from A1.
Verify fixture/source/body hashes and create an exclusive LAUNCHED marker. Remove
the run-owned private credential copy in all exit paths. Preserve raw per-attempt
transport evidence, raw final answers, timing and request identities.

Report 3x3 confusion matrix for singleton primary labels, separate set-admissible
results, useful-target false negatives, unrelated-target retention, uncertainty and
pairwise invariance/sensitivity by family. Relevant and uncertain both retain;
retention is not strict classification correctness. Equal labels are required for
invariance pairs; sensitivity pairs should yield their frozen opposed labels;
evidence-resolution pairs should yield relevant with the witness and an admissible
non-positive answer without it. No aggregate accuracy hides family or pair errors.

Usage comes from each actual transport response once, never final+response twice.
Missing token/cache/cost fields are unavailable. Report logical latency and recorded
attempt lifecycle timings separately; do not infer provider-only time or transmission.
One pass cannot estimate stochastic stability. No confidence threshold is selected.

This is attribution, not an adoption trial: no post-result implementation/prompt
change. Sufficient-evidence false negatives suggest a semantic-interface experiment;
missing-witness ambiguity suggests evidence-scope work. Existing fixture-judge
starvation remains a separate mechanical finding. No AppWorld rerun.
