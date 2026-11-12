# Bounded discovery classification

## Additive online selection

The live discovery service passes the pinned session's exposed capability set to
the connected selector. Already exposed declarations remain available; they are
not separate contribution questions. Ranked exposed declarations can anchor
missing suppliers, and direct exposed suppliers remain question-local evidence
for new consumers. Evidence still counts toward the unchanged 16,000-byte
packet limit and one logical judgment. Exposure means a declaration is available, not that its
acquisition has executed or that it is currently relevant or authorized to run.

Only new candidates receive contribution judgments. Required prerequisite closure
and authorization remain intact. A fully exposed catalogue requires no provider
request. Zero additions to an existing surface does not imply an unmatched or
completed workflow. Canonical exposure membership participates in selector cache
identity, so a larger surface cannot reuse an earlier selection delta. Catalogue
and authorization validation still precede cache admission.

Within each session, completed judgments advance a durable exploration frontier.
Matching candidates with fewer completed judgments take priority; their existing
direct consumers of exposed producers take priority within that round, followed
by current-need lexical order. Only existing typed supplier and creation edges
nominate consumers; no transitive expansion or automatic exposure occurs. The
same two initial root slots and packet budget apply. This is allocation priority, not a
negative relevance cache: previous rejections can be reconsidered, never remove
declarations and never grant execution permission. Session, catalogue generation,
authorization and model scope the frontier. Duplicate completions advance it once.

The current discovery request is self-contained: callers include relevant
conditions and amendments. It is the judgment outcome; earlier requests are not
model evidence. Lossless ancestry remains in audit receipts, continuation validation
and search identity. Removing it from judgment does not reset spent work or the
session frontier. Full target contracts and question-local composition evidence
still count toward the unchanged packet limit.

Root retrieval uses the current discovery need, without rank fusion against the
original outcome. Conditions and amendments in the current request reach judgment
unchanged; discovery does not reconstruct them from earlier requests.
Authored fields use the existing catalogue lexical normalization, preserving short
domain terms. Identical input annotations count once in retrieval, regardless of
how many slots reuse them; every slot remains in the complete contract. Parent
GETs index their authored embedded-relation descriptions;
operations sharing the entity do not inherit those observations. These changes
do not rewrite complete contracts or infer bindings from names.

Discovery's lexical key also normalizes terminal `e` after suffix removal so
authored `calibrate` and requested `calibrating` match. This local retrieval rule
does not alter language correction, catalogue identities or typed bindings.
Operation descriptions and declared effects both remain searchable: a generic
effect description must not erase the operation's more specific authored purpose.

An optional route's budget or acquisition-cycle failure does not reject its
capability as an independently ranked root. Direct admission is measured
separately with full target evidence and required closure. Unranked direct
consumers may be nominated by an exposed producer; other unranked neighbours
are not promoted to roots. An admitted direct root clears its earlier deferral.

The contribution rubric is unchanged. The scoped-classification
API described below remains distinct from the live connected selector.

## Concurrent search ownership

Transient provider or connectivity failures may retry the identical judgment,
at most three attempts within 30 seconds, with exponential backoff, jitter and
Retry-After respected within that deadline. Each attempt is reserved and
checkpointed before dispatch: at most 48,000 admitted UTF-8 bytes. Receipts count
all admitted attempts and missing usage; cancellation never refunds admission.
An admission can be persisted before cancellation prevents transmission, so
admission and dispatch-start receipts do not prove delivery to the provider. Malformed
responses, context limits, authorization and persistence failures do not retry.
No API execution or business effect is retried by this policy. Selection,
contracts and contribution instructions are unchanged. The v13 cache identity
separates current-request judgments from the earlier original-outcome projection.

### Transport resilience and evidence

Discovery injects a `DecisionTransport` that performs exactly one judgment attempt.
The reqwest adapter and abstract transports cross the same interface. Provider
admission is a decorator inside that attempt, so queue time is bounded and
distinguished from dispatch and response-body time. The selector alone owns
durable attempt/byte admission and the overall 30-second deadline. Each discovery
transport invocation has an eight-second timeout, including admission. A stalled
first attempt therefore returns a typed, retryable timeout before the logical
budget expires. This slice is a conservative policy choice, not an empirically
optimal timeout. Slow successful judgments can be cancelled and cost usage even
when no response is received; cancelling a request does not cancel provider work.

Runtime HTTP and decision transport share `external_resilience::RetryPolicy` for
bounded exponential backoff, jitter, retry admission and `Retry-After` parsing.
Jitter never shortens a server hint. Domain HTTP retries remain restricted to
GET/HEAD/OPTIONS; a repeatable model POST does not authorize replay of API writes.
Provider-specific status classification remains in each adapter's policy. A
partially received 401/403 body remains terminal even when body reading times out.

When `PLASM_DISCOVERY_REJECTION_DIR` is configured, lifecycle records include a
stable `attempt_id`, start timestamp, elapsed milliseconds and the phase
(`admission`, `dispatch`, `response_body`). Completion, failure and cancellation
are recorded separately from response/transport receipts. Count terminal attempt
receipts once; do not sum lifecycle events as requests or usage. Status and
Retry-After survive a body timeout. Drop records cancellation when an outer
deadline or caller cancels the future; persistence failure during drop is logged
because cancellation cannot return an error. A process crash cannot run this
cleanup: an unmatched start is unfinished work, not proof of a transmitted request.
No admission or byte reservation is refunded by cancellation.

Identical searches across stores or workers wait on a PostgreSQL transaction
advisory lock before checking completion or reserving work. Ownership spans the
completed receipt publication. A follower then reuses that receipt rather than
treating a live owner's durable reservation as interrupted work.

The lock represents liveness; durable reservation and progress records represent
spent work. Owner failure or cancellation releases the lock without deleting
those records or authorizing another provider attempt. Cancelling a waiter does
not cancel the owner. At most half of each store's database connections may be
held by search ownership, leaving capacity for checkpoints and completion.

## Scoped classification API

The scoped-classification API accepts bounded retrieval receipts. Authorization
precedes classification; callers determine its candidate set.

Capability relevance uses native OpenRouter Decisions, default
`openai/gpt-6-luna-decisions`; an eight-digit dated revision may be configured.
There is no whole-environment support classifier.

## Local contribution evidence

Each question judges exactly one retrieved capability. It carries that target's
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

Receiver acquisitions are shown on the consuming operation's question. The
acquisition's own complete output already declares its entity identity; it does
not import every method accepting that entity. A method is not its own receiver
acquisition. Ordinary argument and membership links remain unchanged, including
distinct compatible sources; dense genuine scopes remain subject to the same bounds.

Exact reciprocal membership endpoints share one fact marked `bidirectional: true`.
One-way membership and distinct fields or relations remain separate; this does not
coalesce input flow or prerequisites. Keyed record fields and relations omit an
internal `name` only when it equals their key. All other contract metadata stays
verbatim, including requiredness, descriptions, defaults, nulls and materialization.

Matching wire profiles (for example, email or telephone formatting) alone do not
establish input or membership relationships. They remain in each target's full
contract for classification and validation. Distinct named roles do not become
bindings because their formats match. Membership requires an entity key and
shared authored identity or exact entity reference. Shared authored value identities,
typed entity references, receivers, relations and declared prerequisites still
supply composition witnesses. This does not remove retrieved candidates or forbid
programs from composing format-compatible values; it avoids presenting format
compatibility as an inferred catalogue relationship.

Declared prerequisites are resolved before classification to construct evidence,
including evidence-only providers absent from retrieval. Every endpoint must pass
the same capability-level authorization check. Supplemental providers do not
become business candidates or gain judgments. Missing deployment bindings, cycles
or prerequisites without output bindings fail before provider work; they are not
silently omitted. This is earlier failure than post-selection execution closure.
Successful selection still runs normal prerequisite closure and execution admission.
Prerequisite witnesses are directed toward the consuming capability: judging an
acquisition does not pull in all consumers of that acquisition. Closure retains
the acquisition whenever a selected consumer needs it. Ordinary business-data
witnesses remain visible from both ends.

## Bounds and atomicity

All local requests are constructed and checked before any provider call.
Each original local scope is limited to **16,000 encoded bytes and eight capabilities**.
Valid scopes exceeding either bound are retained as **uncertain**, without a
provider call. The receipt's typed `budget_retained` entries record candidate ID,
capability reference, measured count/bytes and both limits. These are host budget
outcomes, not model judgments, probabilities or provider usage. Fitting scopes
still receive normal classification. No truncation, arbitrary neighbor ranking,
larger-context retry or whole-catalogue evidence substitution occurs.

`prepare_selection` validates every scope, separates fitting requests from
budget outcomes, and packs fitting scopes into requests of at most four questions
and 16,000 full-body UTF-8 bytes. A merge must be smaller than its separate requests.
Only exact common context and whole type definitions are shared. Every question's
remaining cards, definitions, references and bindings stay in its own instructions;
no union of sibling evidence is placed in shared state. Recombining shared state
with one question's local evidence reconstructs its exact original scope.
Singleton bodies and cache identities remain unchanged. Live judging and sequential `SelectionPages::from_prepared`
consume both. Public execution APIs carry the complete prepared selection rather
than a request-only vector that could lose local outcomes. Invalid evidence and
unauthorized bindings abort preparation before any
dispatch; budget retention catches only the typed size/count admission outcome.

A provider context rejection fails the route without subdivision or replay. A complete
selection requires one validated judgment or explicit budget outcome for every
original retrieved candidate. Missing answers, malformed responses and provider
faults still abort; they never become budget uncertainty. Selection returns no
partial receipt. Full declarations, authorization and prerequisite closure are
unchanged for retained candidates.

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

Complete request bytes and framing version `decisions-scoped-sharing-v11`
determine packed cache identity; unchanged singletons retain `decisions-compact-witnesses-v10`.
Changed local evidence or intent invalidates that request.
There is no prompt padding or provider-error fallback. Budget outcomes are
recomputed from the current pinned evidence, not cached as provider responses.
Capability IDs remain scoped to the complete candidate/evidence set. Adding or
removing an earlier-sorted reference can renumber a local request and invalidate
its cache key even when its semantic evidence is unchanged.

| Boundary | Limit / behavior |
| --- | --- |
| Local question scope | Eight capabilities, original 16,000-byte admission unchanged |
| Request | At most four questions; 16,000 full-body UTF-8 bytes; strictly smaller than separate bodies |
| Route | At most four admitted provider futures |
| Process | At most eight provider requests |
| Queue | FIFO semaphore; no fleet-wide or strict per-tenant fairness claim |
| Context rejection | Explicit failure; no subdivision or partial receipt |
| Cancellation / error | Drop outstanding futures and release claims; no success receipt |
| Cache fill | Exact-request coalescing for cloned discovery stores; failures not cached |

Existing bounded transport retry and shared-permit behavior are unchanged. Separate
processes and independently opened stores do not share in-flight claims. Waiting
cache consumers recheck after acquiring a claim.

Discovery does not refresh account data or execute acquisitions. Programs still
perform reads and reviewed execution. Deterministic integration tests establish
projection and scheduling boundaries, not semantic accuracy or catalogue-growth
recall. The offline model probes remain diagnostic evidence, not an AppWorld score.
