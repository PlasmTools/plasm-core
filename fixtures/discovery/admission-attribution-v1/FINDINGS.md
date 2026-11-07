# Attribution: ranking amplification, not a missing catalogue quota

All 98 variants preserve every historical per-turn field, including packet bodies, receipts, decisions, deferrals, exposure and outcome flags. Forward/reverse checks pass. The native diagnostic ranking is asserted equal to normal ranking, and the independently constructed first diagnostic packet equals the dispatched fixture packet. No ranking policy, model prompt, budget or provider calls changed. The additional construction makes these runs unsuitable for speed comparisons.

## What the score trace shows

In the repeated cabinet-membership request, increasing topical distractors from 0 to 100 to 1,000 changes:

| Operation | 0 distractors: rank / score | 100: rank / score | 1,000: rank / score |
|---|---:|---:|---:|
| Needed cabinet read | 3 / 4.21948 | 105 / 0.28326 | 1,005 / 0.03019 |
| Unrelated firing operation | 4 / 2.57857 | 2 / 7.02515 | 2 / 10.72135 |

The firing operation's entire score in this request comes from `workshop`. Its inverse-document-frequency weight rises from 1.89712 to 6.51224. Meanwhile the needed membership read's matching `membership`, `cabinet` and `specimen` terms become common in the distractor population and lose weight. The publication request has the same pattern: publication falls from rank 3, score 9.57840, to rank 1,005, score 0.05926. At 1,000 distractors each historical record scores 0.09605, exceeding publication through broad topical overlap, including terms in the request's explicit exclusion of historical records.

These numeric scores are comparable as diagnostics of this formula, not calibrated probabilities or utility. The field masses are pre-saturation inputs; only the per-term contributions add to final score. The trace establishes multiplicity-sensitive corpus weighting and lexical matching of excluded context. It does not prove that removing IDF alone would fix either failure.

## What allocation does with that ranking

The first 1,000-distractor cabinet packet is 14,765 bytes: two direct roots (stamp and firing), one optional specimen supplier, another direct root, then five historical record roots. The cabinet read is budget-deferred before judgment. Its low rank is upstream of that byte failure.

The corresponding independent-outcome packet is 14,980 bytes. It retains routine, firing, preparation and sample acquisition, then fills with five historical records. Publication remains budget-deferred. Optional paths are not the bulk of the crowding in these examples.

Typed acquisition also supplies a positive control: the service-effect case acquires sample_read at lexical rank 1,005 through its connection to the selected preparation/firing path and completes in one search. Do not remove that path machinery in an attempt to simplify admission. Membership scope and independent publication are different obligations; a generic supplier path cannot stand in for them.

Across all 98 first-search traces there are 196 initial-root admissions, 88 optional-path bundle admissions and 584 remaining-root admissions. These are admissions, not model calls or unique operations across the whole run. The first admission's byte delta includes framing; subsequent deltas measure the whole packed request's increase.

## Design consequence and next experiment

Keep catalogue round-robin rejected and the production scheduler unchanged. The next isolated diagnostic should examine the contribution of corpus rarity to ranking, not add another ownership quota or spend more packet bytes.

One candidate experiment: keep the same term normalization, field roles, saturation, length normalization and complete contracts, but make IDF constant for ranking only. This tests whether rare context dominates because of corpus rarity. It deliberately does not claim to understand negation, infer outcomes, deduplicate operations or solve all starvation. Keep every distinct operation eligible and typed paths unchanged. Freeze this single factor and include fresh controls where rare action words are genuinely discriminative, alongside the 98 development variants. Report both improvements and loss of rare-action discrimination. Do not adopt it merely because cloned-distractor cases improve.

If broad topical records still outrank the requested effect, that residual is an action/context representation problem requiring a separately isolated design. If highly ranked useful paths are displaced, inspect admission competition. Caller use, program repair and terminal completion remain outside this model-free attribution.

This finding is post-hoc diagnosis of fixed development evidence. No new intervention has been run or approved for adoption.
