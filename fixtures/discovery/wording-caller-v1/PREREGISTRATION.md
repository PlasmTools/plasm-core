# Wording caller v1 — exploratory mock-environment experiment

2026-11-01. User authorised run. Freeze before provider calls. Hypothesis: exposing
recorded search facts reduces futile rediscovery and false absence claims without
discouraging useful continuation or use of already available operations.

Single factor: current formatter versus receipt-derived wording from the offline
check. Same tasks, capability descriptions, deterministic search schedule, execution
results, caller instructions, tools and budget. Candidate preserves teaching and
replaces the generic extend-discovery imperative. No production changes.

Five neutral development scenarios: available read; useful continuation; stalled
byte deferral; bounded negative judgment; genuinely changed objective. Two repeats
per arm (20 trajectories); arm order alternates by case and repeat. Maximum six
caller generations per trajectory (120 total). Stop at first terminal response,
invalid output or exhaustion. No task retries or discarded model outputs. Abort
the entire run on unrecovered provider/harness errors and retain unstarted cases.

Model openai/gpt-6-luna via existing OpenRouter resolver, high reasoning, requested
temperature zero, 16384 max output tokens, 300000ms per generation, SDK maxRetries0.
Existing resolver retries explicit rejected 429 responses only (max8/120s); record
wire attempts separately and never count them again as SDK usage. No model choice
or prompt changes after results. No embeddings, native discovery provider or real
effects. Mock capability execution isolates decision-making from program syntax.

Judge: for available/useful/changed cases, execute the requested operation and
finish with the exact returned status; for permanently stalled/negative cases,
finish blocked without a catalogue-absence assertion. Unavailable is never justified
by these partial receipts. Report completed-correct, blocked-correct, premature
completion, false-unavailable, unsupported execution, turns, discovery calls,
post-exposure discovery calls, exhausted/invalid outputs, usage, latency and cost
when recorded. For useful continuation, a blocked terminal counts as missed useful
continuation. Every row reported; no aggregate alone is a promotion gate.

Offline controls must prove deterministic search advances on the intended call,
execution rejects unavailable operations, target output is required for success,
bounded negatives do not imply absence, and exact teaching is retained in both
wording arms. Freeze harness, prompts/cases/judge, formatter and provider sources.

All tasks are development diagnostics, not holdouts. This is a small caller-policy
study, not a native discovery, full Plasm agent or AppWorld result. Natural-language
claims beyond structured terminal status require manual review; do not silently
score text with a keyword classifier. No promotion or further arms in this run.
