# Pre-registration: direct verified connection rendering

Hypothesis: placing a mechanically rendered source-field to consumer-input
connection inside the question scope helps the unchanged model resolve supplier
contribution without asserting semantic purpose or execution authority.

Single factor: candidate adds `verified_connections` beside each question's cards,
derived solely from that scope's existing binding_refs and binding_witnesses.
Example grammar: `c0.id -> c1.arguments["token"]; collect=false`. Original witnesses,
descriptions, full contracts, references, rubric and requests remain byte-equivalent
after removing this one added field. No evidence crosses question scopes. Missing
witness cases add nothing, producing identical baseline and candidate bodies.

Legal class: deterministic renderer. Targets and local witnesses are fixed diagnostic
fixtures, not a retrieval or product-performance benchmark. No gold answer or inferred
purpose enters the renderer. No production implementation change or AppWorld run.

Development evidence: decision-interface-v1, already consumed. Fresh validation:
24 connection-v2 cases frozen before calls, six families with four cases each:
support, different outcome, prohibited effect, opaque consumer, absent witness,
direct effect. These fresh controls are diagnostic-only, not an independent hidden
holdout or 24 independent domains. No headline generalization claim is permitted.
Descriptions and labels are manually authored before scores; they remain fixed.

Two arms, 24 paired comparisons, 48 logical calls total. Same native model
openai/gpt-6-luna-decisions, contribution rubric and decoder. No new temperature,
effort or sampling override; use the exact existing Decisions request surface.
Alternate baseline-first and inline-first by case index to balance order. One
sample per arm per case; no quality retries, further arms or tuning after results.

Limits unchanged: each body <=16000 UTF-8 bytes; one logical judgment per body;
<=3 identical transient transport attempts within30s, 8s per attempt including
admission, <=48000 bytes per logical request. At most48 logical requests and
2,304,000 attempt-body bytes. First unrecovered fault aborts, no silent resume.
No effects, embeddings, provider probes or AppWorld evaluation.

Freeze labels, inputs and source before export; validate exact A/B deltas, typed
witness references and byte ceilings. Export and existing active matcher tests
must pass before launch. Freeze exact bodies before provider access. Store raw
attempts, responses and logical timings; charge each response once. Missing
usage, cache or transmitted-byte evidence remains unavailable. Remove private key.

Primary endpoint: strict relevant answers on the four support cases, reported as
paired recoveries/regressions. Adoption signal requires at least two strict
recoveries, zero strict support regressions, zero new unsupported relevant answers
in different-outcome/prohibited/opaque/absent controls, and no lost useful retention.
Direct controls must remain relevant. All conditions must hold; otherwise reject
adoption. Tiny n and repeated noun structures mean any signal requires a separate
larger sealed validation before production adoption.

Report singleton confusion separately from admissible-set matches. Suppliers with
only unrelated/forbidden consumer evidence admit uncertain/unrelated; positive
relevance is unsupported. Opaque requires uncertain. Absent witness admits
uncertain/unrelated despite hidden global utility. Both relevant and uncertain
retain, so report actual useful loss and unrelated retention separately from
label errors. Record identical-body controls to expose stochastic changes. Raw
confidence is descriptive only; no threshold selection. No post-hoc exclusions.

Forbids 1–7 checked: no labels in model input, no goal-solving scaffold, no tuning
on reported controls, no per-failure rules, two predeclared arms only, model/rubric
unchanged, all families/metrics reported. This is a diagnostic A/B, not product win.
