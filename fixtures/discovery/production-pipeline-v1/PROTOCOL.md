# Fixed discovery production pipeline, development run

## 2026-10-31 exact-evidence scheduling measurement

Single new arm v16: within the existing exposure/visit tiers, schedule one member
per exact authored evidence document before repeated copies, independently in both
recall channels. Capability identity is the only excluded document field; retain
every candidate identity, full contract, authorization and independent judgment.
No grouping by entity name, inferred intent, task labels or approximate similarity.

Compare with sealed fixed-production-frontier-v15-20261031 and original v14.
Freeze all 34 requests/labels, models/decoding, budgets and 24-search horizon.
Reuse the same catalogue vectors. No changes after results and no automatic rerun.
Primary speed criterion: improve four-search completion above 20/34 without losing
v15's 31/34 horizon coverage or any of its four-search successes. Also measure
searches and summed whole-turn milliseconds through first completion, paired only
where both runs complete; report unresolved cases separately rather than treating
them as fast zeroes. Report actual provider attempts/tokens/cost, full-horizon
latency, and extra exposure. This remains exploratory development evidence.
Earlier sections below retain the historical preregistrations and preparation
errata; they do not authorize additional arms in this run.

## 2026-10-31 frontier-progress measurement

Single new arm: production v15 applies existing completed visits and exposure before
each bounded recall cutoff. Compare all 34 paired variants with the sealed
fixed-production-pipeline-20261031-summary baseline (v14). Frozen fixture locks,
requests, labels, model, decoding, budgets, order and 24-search horizon stay fixed.
Reuse baseline catalogue embedding files by content hash. Query embeddings use the
same configured profile in a fresh database. No completed-case retries or additional
arms. Stop on the first unrecovered fault and report partial evidence.

Primary development criterion: improve completion by search four from 20/34 without
losing previously passing cases. Also report completion by search 24, paired first
completion and no-gain changes, extra exposure, failure stages, actual provider
usage and whole-turn latency. A single stochastic repeat cannot establish causal
or holdout performance. Earlier preparation errata below describe the sealed
baseline; this arm starts all 34 variants with the repaired, validated harness.

Hypothesis: the repaired hybrid recall and production judgment expose the fixed
requested paths without the starvation hidden by lexical-only/oracle harnesses.

One current-production condition, no tuning or additional arm after results.
Preparation erratum, before any provider call: original mappings fail native
publication with UnwiredCapabilityParameters. The versioned discovery_pipeline_v1
fixture wires the four existing parameters to abstract HTTP bodies. The original
domain.yaml is byte-identical; the original locked fixture is untouched. Preflight
asserts exact capability-document parity before/after mapping substitution and
compiles the repaired mappings. No discovery annotation, label or request changes.

Unstarted-cohort continuation erratum: the first live attempt completed exactly six
zero-growth trajectory variants, then publication rejected the generated journal's
orphan entities. Preserve those results; never rerun them. The harness now removes
entities with no capabilities in generated catalogues, asserts exact capability
document parity, and preflights full CGS IL loading as well as template compilation.
Continuation runs only the remaining 28 variants with the same requests and budgets.
Prior catalogue vectors are reused by content hash, not reacquired or charged twice.
This is an explicitly exploratory preparation repair after partial results.
Fixtures and labels: unchanged trajectory-v4 (18 variants) and action-role-pairs-v1
(16 variants), checked against their original locks. All are consumed development
data, not holdouts. The semantic unit is a case family, not an independent trial
for each growth or paraphrase variant. Labels never enter recall or judgment.

Preserve the existing 24-search horizon, four-search availability gate, growth
levels, self-contained requests and outcome/relation labels. Single forward order;
do not demand deterministic equality from live model repeats. No forced rejection
arm, query rewriting, task boosts, executed business effects or AppWorld agent run.

Actual pipeline: canonical CGS documents -> configured real catalogue embeddings
with content-addressed cache -> validated publication/import -> isolated pgvector
database -> DiscoveryService::route_turn -> exact query/profile cache and real query
embeddings -> lexical/vector union -> native allocation -> real Decisions transport
and judgment -> durable frontier -> additive exposure and native teaching.
Read-only recall diagnostics run after each timed turn, using the populated cache.

Model: openai/gpt-6-luna-decisions, production decoding and decision contract.
Embeddings: existing default EmbeddingProfile. No prompt/model changes. One logical
judgment/search, <=16000-byte packet, <=3 identical transient attempts within 30s,
8s per attempt, <=48000 admitted bytes. No task/case retries. Maximum 816 turns.
Missing catalogue and query embeddings are acquired for these abstract fixtures;
their actual response usage is retained separately from discovery judgment.

Success criterion: report fraction complete by search four using unchanged labels;
report first-complete search, no-gain prefix, extra retained capabilities, per-target
recall absence / allocation / budget deferral / model rejection, requests, bytes,
latency and provider-reported usage. Missing usage is unavailable. Dispatch does not
prove transmission. Never sum lifecycle and response receipts as separate requests.
Retaining an excluded operation does not authorize executing it.

Stop at the frozen horizon; abort first unrecovered transport, database, decoding
or harness error. Preserve partial receipts and report unobserved cases explicitly.
No automatic reruns. Source and fixture hashes checked before and after execution.
Only the uniquely owned disposable database and copied credential file are removed.

Old oracle/lexical results are component baselines, not matched causal controls.
This run changes the exercised pipeline in several ways; it establishes current
development behavior, not a causal effect, holdout score or full-agent readiness.
