# Bounded discovery classification

Retrieval is unchanged: up to 64 lexical and 64 vector candidates, deduplicated
to at most 128. Authorization is applied before classification. Already exposed
capabilities are still classified against the current intent and full intent ancestry.

Capability relevance uses the native OpenRouter Decisions API, default model
`openai/gpt-6-luna-decisions`. `PLASM_DISCOVERY_CAPABILITY_MATCH_MODEL` may pin
an eight-digit dated revision of that model. JEV and chat-model overrides fail
explicitly. There is no whole-environment support classifier.

Each request contains shared evidence and independent questions:

```json
{
  "model": "openai/gpt-6-luna-decisions",
  "state": {
    "intent_turns": ["Inspect selected records"],
    "outcome": "Inspect selected records",
    "discovery_need": "Inspect selected records",
    "rule": "<complete contribution rubric>",
    "capabilities": {
      "c0": {
        "capability": {
          "reference": {"catalog": "matrix", "capability": "inspect"},
          "entity": "Record",
          "operation": "<complete structured operation evidence>"
        },
        "collection_meaning": "<complete collection meaning>"
      }
    }
  },
  "questions": {
    "q0": {
      "type": "choice",
      "instructions": "Classify capabilities.c0 using the shared intent and rule.",
      "criteria": {"relevant": "...", "uncertain": "...", "unrelated": "..."}
    }
  }
}
```

The illustration abbreviates instruction text. The real operation field retains
the full typed evidence, including input, scope, prerequisites and effects.
Each complete contract occurs once per request in shared state. Each question
explicitly references its subject there; it does not depend on sibling questions
being visible. Criteria are per-question as required by the native API.
All pages of a discovery turn retain the same bounded retrieved evidence set,
including during context-driven subdivision. A prerequisite on another page
therefore remains visible. This does not recover evidence absent from retrieval.

The rubric judges documented output, effect or information contributing to the
requested outcome. Relevance does not certify current applicability, permission,
or the existence of a valid workflow. Underspecified contribution remains uncertain.

Native responses contain a keyed `answers` map. The shared typed decoder validates
model/provider identity, exact question coverage, duplicate keys, choice types,
allowed choices, confidence and probability vectors. Only the choice enters the
routing receipt; probability metadata does not introduce a selection threshold.
No partial selection is committed. Final receipt order is independent of response order.

Shared evidence is ordered by SHA256 of canonical complete evidence JSON, with stable
wire identity breaking ties, independently of each page's subjects. The
serializer preserves this map order directly; an intermediate JSON map must not
sort it away. Every page, including split pages, retains byte-identical shared
state and every complete contract. Question identities remain stable across splits.
Incoming retrieval permutation does not change request bytes.
This removes incidental ordering differences, not model response variability.
Relevant and uncertain capabilities are both retained; no confidence threshold,
extra classification request or precision-only rejection is introduced.

Complete request bytes and framing version `decisions-shared-contribution-v6` determine
cache identity. Changing any shared contract invalidates every page that uses it.
There is no chat schema, generated reasoning, prefix padding, or automatic fallback.

| Boundary | Limit / behavior |
| --- | --- |
| Page packing | At most 128 questions; 64,000 serialized subject-evidence bytes target, excluding common state; not a token/context guarantee |
| One discovery route | At most 4 admitted page futures, including split work |
| One process | At most 8 provider requests |
| Queue admission | FIFO semaphore; no fleet-wide or strict per-tenant fairness claim |
| Context rejection | Split only rejected questions; preserve the complete shared state; never repeat successful pages |
| Indivisible contract | A singleton question plus shared evidence is indivisible; fail explicitly if rejected |
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
permits/cache fills, lossless evidence and same-entity teaching extensions. Isolated model probes measure the compact framing separately; deterministic tests
do not establish semantic accuracy or retrieval recall under catalogue growth.
