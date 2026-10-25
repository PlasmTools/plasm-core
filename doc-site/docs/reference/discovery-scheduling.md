# Bounded discovery classification

Retrieval is unchanged: up to 64 lexical and 64 vector candidates, deduplicated
to at most 128. Authorization precedes classification. Already exposed capabilities
are still judged against current discovery need and full intent ancestry.

Capability relevance uses native OpenRouter Decisions, default
`openai/gpt-6-luna-decisions`; an eight-digit dated revision may be configured.
There is no whole-environment support classifier.

## Local contribution evidence

Each request judges exactly one retrieved capability. It carries that target's
complete catalogue declaration and referenced definitions, plus compact witnesses
for directly connected catalogue input, receiver, membership and prerequisite
bindings. Neighboring operations retain their authored description and exact
matched slot metadata. Output metadata remains at `declaration.output`; it is
never inferred from transport mappings or an entity name.

Unrelated neighboring fields and whole neighboring records are not included.
Named slot definitions are deduplicated; nominal references do not recursively
expand neighboring records. Target definitions remain complete. Defaults, input
validation, union variant absence, receiver identity and effect descriptions are
preserved. An invalid selected slot fails explicitly. Union, inline-array and
anonymous-value inputs retain their full target contracts, but do not invent
source edges where a branch or element binding is not established.

This is direct binding scope, not exhaustive workflow inference. The existing
source projector supplies required non-prerequisite read-to-input/receiver
relationships. It does not infer every optional-input or action-output composition.
Peers of a neighboring capability are not traversed.

Declared prerequisites are resolved before classification to construct evidence,
including evidence-only providers absent from retrieval. Every endpoint must pass
the same capability-level authorization check. Supplemental providers do not
become business candidates or gain judgments. Missing deployment bindings, cycles
or prerequisites without output bindings fail before provider work; they are not
silently omitted. This is earlier failure than post-selection execution closure.
Successful selection still runs normal prerequisite closure and execution admission.

## Bounds and atomicity

All local requests are constructed and checked before any provider call.
Each complete UTF-8 request is limited to **16,000 bytes and eight capabilities**.
If any scope exceeds either bound, `ScopeLimit` identifies the target, actual
count/bytes and limits. The route does not dispatch partial work or return a
partial selection. No truncation, arbitrary neighbor ranking, larger-context retry
or whole-catalogue evidence substitution occurs.

A provider context rejection of a local question is indivisible and fails the
route. Successful judgments are not replayed as part of subdivision. A complete
selection requires one validated judgment for every original retrieved candidate.

The unchanged rubric asks whether documented output, effect or information can
contribute to the outcome. This does not certify readiness, permission or a valid
workflow. Relevant and uncertain choices are retained; no probability threshold
is introduced. The typed decoder checks exact question coverage, duplicates,
choice types, probabilities and model identity.

## Stable serialization and scheduling

Local capability cards retain canonical complete-contract hash order, with
deterministic reference identities breaking ties. Definition sections retain
values-before-records wire order. Retrieval permutation does not change bytes.
The model sees only the local projection; a single internal definition serialization
is reused while assembling the route's requests.

Complete request bytes and framing version `decisions-compact-witnesses-v10`
determine cache identity. Changed local evidence or intent invalidates that request.
There is no prompt padding or automatic fallback.
Capability IDs remain scoped to the complete candidate/evidence set. Adding or
removing an earlier-sorted reference can renumber a local request and invalidate
its cache key even when its semantic evidence is unchanged.

| Boundary | Limit / behavior |
| --- | --- |
| Request | One question, eight capabilities, 16,000 full-body UTF-8 bytes |
| Route | At most four admitted provider futures |
| Process | At most eight provider requests |
| Queue | FIFO semaphore; no fleet-wide or strict per-tenant fairness claim |
| Context rejection | Explicit indivisible-local-question failure |
| Cancellation / error | Drop outstanding futures and release claims; no success receipt |
| Cache fill | Exact-request coalescing for cloned discovery stores; failures not cached |

Existing bounded transport retry and shared-permit behavior are unchanged. Separate
processes and independently opened stores do not share in-flight claims. Waiting
cache consumers recheck after acquiring a claim.

Discovery does not refresh account data or execute acquisitions. Programs still
perform reads and reviewed execution. Deterministic integration tests establish
projection and scheduling boundaries, not semantic accuracy or catalogue-growth
recall. The offline model probes remain diagnostic evidence, not an AppWorld score.
