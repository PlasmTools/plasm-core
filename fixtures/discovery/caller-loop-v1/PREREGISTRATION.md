# Caller-controlled native admission loop v1

Fresh exploratory development trajectories; no sealed holdout or AppWorld run.
Ten cases, one model arm, at most six discovery calls per case. Freeze cases,
prompt, native helper, caller and judge before any provider call. No post-result
fixture or prompt tuning. A failed geometry assertion invalidates the fixture;
do not silently resize contracts. Any follow-up uses new registered cases.

The actual existing caller model (openai/gpt-6-luna, high reasoning, temperature
0, 16,384 output tokens, 300,000 ms) chooses plasm_context intent and the proposed
continue/revise field using real tool calling and accumulated responses. It may
stop by responding without a tool call. Sequential requests, no semantic retries,
outer SDK retries disabled, existing bounded explicit-429 recovery unchanged.
Abort the whole run on unrecovered infrastructure/provider fault; preserve all
completed, interrupted and unstarted cases. At most 60 logical calls.

Native ordering/allocation, exact packet renderer and 16,000-byte cap are real.
Retrieval order is deliberately frozen; relevance is an oracle keyed to the
current user objective. This isolates caller-controlled admission, NOT the full
production retrieval/judgment pipeline, provider effect execution or task success.
Each numbered worker RPC represents a fresh logical search. The current service's
cache-key, budget-admission and concurrent-owner paths are not invoked. No claim
about production continuation or duplicate-request idempotency follows from this
worker; those require separate integration gates before a production cutover.

Generated abstract capabilities preserve independent action semantics: the
requested action applies a violet inspection seal to specimen units; an initial
competing operation applies an amber coating. Other operations perform unrelated
unit operations. Their contract paragraphs repeat the same preservation
constraint 80/110/140 times as specified. Existing abstract typed process/supplier
fixtures are retained only in connected cases. No production catalogues.

Initial objective: "Apply a violet inspection seal to specimen units. Preserve
their existing measurements and identifiers." On registered amendment call,
replace it with "Apply an amber coating to specimen units instead of a violet
inspection seal. Preserve their existing measurements and identifiers." The
previously rejected first root now supplies the required operation. On an
observation call, add "Unit X has an incomplete measurement record. The existing
measurements and identifiers must remain intact." The requested action is unchanged.

For each case, use the native allocator first with correct signals and original
goal text as a model-free upper-bound control. Verify both competing roots fit
alone; verify their combined overflow except the 80-paragraph ordinary control.
Do not expose ranks, target IDs, scenario names, future events or oracle results
to the caller. The model receives only the current goal, subsequent observations
and prior packet judgments. Stop each trajectory on first admission of the
current goal's relevant root; this measures availability, not executed success.

Judge: expected first signal Revise; subsequent signals Continue except registered
amendment, which expects Revise. Invalid/multiple tool calls, missing fields,
wrong tool name, or premature text termination are case failures, never repaired.
Record false resets, missed revisions, target arrival, repeats, packet bytes,
tokens/latency/faults and per-case loss against the native correct-signal control.
Manually audit request fidelity using a frozen rubric: action, specimen-unit
scope and preservation of both measurements and identifiers must remain stated;
new observations must not silently authorize new effects. Report fidelity
violations separately even if oracle admission succeeds. Desired gate: all ten
available within six, no fidelity violations, no unexplained loss versus control.

No extra classifier is deployed. Model-free oracle judgment is explicitly not
evidence that the production decision model would accept the same packet.
