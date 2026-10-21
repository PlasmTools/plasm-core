# Bounded discovery classification

Retrieval is unchanged: up to 64 lexical and 64 vector candidates, deduplicated
to at most 128. Authorization is applied before classification. Already exposed
capabilities are still classified against the current intent and full intent ancestry.

The classifier user message has this shape (illustrative references):

```json
{
  "context": {
    "current_intent": "Inspect selected records",
    "intent_turns": ["Inspect selected records"],
    "intent_interpretation": "<complete interpretation rule>",
    "rule": "<complete contribution rubric>",
    "criteria": {"relevant": "...", "unrelated": "...", "uncertain": "..."}
  },
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
prerequisites and effect descriptions are not truncated. The strict response remains:

```json
{"answers":{"q0":{"choice":"relevant"}}}
```

Every issued key must occur exactly once; choices are `relevant`, `unrelated`,
or `uncertain`. Missing, foreign, duplicate or malformed answers fail selection.
No partial selection is committed. Final receipt order does not depend on response order.

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
