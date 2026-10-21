# Bounded discovery classification

Retrieval is unchanged: up to 64 lexical and 64 vector candidates, deduplicated
to at most 128. Authorization is applied before classification. Already exposed
capabilities are still classified against the current intent and full intent ancestry.

The classifier messages are ordered as follows: a fixed system instruction,
shared rules, intent ancestry followed by current intent, then page-specific
questions. Each of the three data sections is a separate user message, with no
padding. Illustrative message contents follow:

```json
{
  "criteria": {"relevant": "...", "unrelated": "...", "uncertain": "..."},
  "intent_interpretation": "<complete interpretation rule>",
  "rule": "<complete contribution rubric>"
}
```

```json
{
  "intent_turns": ["Inspect selected records"],
  "current_intent": "Inspect selected records"
}
```

```json
{
  "questions": {
    "q0": {"instructions": {
      "capability": {"reference": {"catalog": "matrix", "capability": "inspect"},
        "entity": "Record", "operation": "<complete typed operation evidence>"},
      "collection_meaning": "<complete collection meaning>"
    }}
  }
}
```

The real operation field is the unchanged structured operation evidence, not a
summary string. Context and criteria occur once per page. Contracts, input scope,
prerequisites and effect descriptions are not truncated. The strict response is:

```json
{"answers":[{"key":"q0","choice":"relevant"}]}
```

Every issued key must occur exactly once; choices are `relevant`, `unrelated`,
or `uncertain`. Missing, foreign, duplicate or malformed answers fail selection.
The provider schema is identical for every page size and intent: it declares an
array of strict objects with required string `key` and enumerated `choice` fields.
The host enforces the exact issued key set and rejects duplicates before admitting
any result. Array position does not identify a candidate. The former answer-map
shape is rejected. No partial selection is committed. Final receipt order does
not depend on response order.

Stable rules precede changing intent; chronological ancestry precedes the current
need. Message text preserves this order even though the outer request object is
canonically serialized. Complete request bytes and framing version v3 determine
the decision-cache key, so old-format responses cannot satisfy new requests.
Separate messages expose reusable prefix boundaries without serial warm-up calls
or explicit provider cache options. Provider cache minimums, routing and cold
parallel requests still govern actual hits: these structural tests do not prove
cache reuse or lower cost. No irrelevant content is added to reach a cache minimum.

| Boundary | Limit / behavior |
| --- | --- |
| Page packing | 16,000 serialized request bytes target, not a token/context guarantee |
| One discovery route | At most 4 admitted page futures, including split work |
| One process | At most 8 provider requests, shared by relevance and advisory support |
| Queue admission | FIFO semaphore; no fleet-wide or strict per-tenant fairness claim |
| Context rejection | Split only the rejected page into complete questions; never repeat successful pages |
| Indivisible contract | Preserve it intact; fail explicitly if the provider rejects it |
| Cancellation / error | Drop pending route futures and release permits/claims; no success receipt |
| Cache fill | Exact request keys coalesce across services sharing a cloned discovery store |

The process permit remains held during bounded transport retries. Separate processes
and independently opened stores do not share in-flight cache-fill claims. Durable
cache keys include the complete model request and a new framing version; failures
are not cached. Waiting consumers recheck the cache after acquiring a claim.

Native teaching reports `declarations_changed` separately from touched entity refs.
The agent distinguishes initial, additional and unchanged declarations while retaining
prerequisite guidance. Discovery never refreshes account data; programs use exposed
reads to observe data. This feedback does not certify task completion or applicability.

Abstract tests cover coverage, splitting, parallel admission, cancellation, shared
permits/cache fills, lossless evidence and same-entity teaching extensions. They do
not establish model-output equivalence under the compact framing or retrieval recall
under catalogue growth. No live evaluation is part of this change.
