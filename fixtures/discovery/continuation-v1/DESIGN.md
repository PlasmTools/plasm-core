# Continuation design boundaries

Discovery adds declarations to a session. Exposure is neither evidence that an
operation is relevant now nor evidence that it ran. A previous judgment answers
a particular request; it is not a permanent capability classification.

## Allocation

Hybrid recall owns current-request ordering. Graph edges nominate additional
candidates and establish typed paths; they must not universally promote an old
producer's consumers over a newly recalled independent outcome. Preserve the
two initial roots and exact packet accounting. Within a visit round, give the
leading unexposed recalled root first priority, then connected consumers, then
remaining roots. This preserves an opportunity for both changed needs and path
continuation. Connected supplier allocation and mandatory prerequisites remain.

This is a bounded opportunity, not a completeness guarantee: two independent
outcomes or a high-ranked visited candidate can still be delayed. The next
factor must measure that rather than claiming starvation has been eliminated.

## Reconsideration (v18)

Separate durable work identity from relevance context. Keep session ownership,
spent bytes, failed attempts, completed receipts and additive exposure intact.
Track judgment visits against the self-contained current request as well as the
catalogue generation and authorization. A different request may reconsider a
rejected candidate; an identical request must retain exploration progress and
reuse its exact completed work. Do not use ancestry length as relevance evidence.

Validate this change with independent stores concurrently: same request
coalesces, changed request can revisit, returning to a previous request reuses
its progress, and failed/cancelled attempts cannot acquire a fresh budget by
changing relevance context. Measure paraphrase-induced repeated work separately.

V18 keys durable exploration frontiers by the exact self-contained current
request in addition to session, generation, authorization and model. It retains
each prior frontier; switching back restores visits. Search-work identity still
includes provenance and canonical exposure, and completion/failed-owner records
are unchanged. This permits reconsideration, but does not guarantee a model will
reverse its rejection. Paraphrases are distinct request contexts; semantic
equivalence is not inferred deterministically.

The leading recall opportunity now advances within the least-visited round.
Previously a rejected leading root continued occupying that priority despite
having moved to a later round, leaving other independent roots behind consumers.

## Execution boundaries

Add an abstract mutable cursor and an independently materialized allowed-value
rowset. A bounded iterate must bind the external stop set and reobserve after
each effect. Assert effect count, final state, zero-step success, empty set bound
exhaustion and no replay after failure. Test both projected and computed sets.

For a tagged union, an absent field remains unobserved, not null. Test that an
unguarded projection fails and a variant-selected projection succeeds. Repair
admission or teaching only if it advertises an invalid operation; never convert
unobserved fields to null to make an evaluation pass.

These execution cases must run through native admission and execution. Planner
recall tests cannot certify them, nor can a passing discovery judgment certify
task completion. All cases are development diagnostics; no agent rerun is part
of this work.
