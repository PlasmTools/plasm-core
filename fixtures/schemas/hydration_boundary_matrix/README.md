# Hydration boundary matrix

Abstract catalog for provider → scoped Get → embedded relation → detail Get.
`access_token` is a capability parameter supplied by `Session.login`, never an
entity identity. `Note.note_id` and `Owner.owner_id` are integer identities.
`Owner._tag` is a public field despite its underscore prefix.

The host tests in `plasm-agent-core/src/plasm_plan_run/tests/hydration_boundary.rs`
round-trip the catalog and compiled program through JSON, execute dry and live
plans against an asserting transport, and repeat execution within one session.
They cover explicit and synthesized Gets, partial embedded rows, delayed replies,
row identity/order/multiplicity, and union of projected and cached rows. A cache
row is also serialized, decoded as relation input, and used for a live detail Get.

The runtime properties additionally exercise multiple parents within one scoped
hydration operation, with different row-scoped parameters and out-of-order HTTP
completion. Cache properties in `cache.rs` use the language matrix for string and
compound identities, duplicate targets, and present-but-empty relations.

Internal relation rows contain typed `_ref` objects plus CGS-typed identity slots.
Display strings such as `Owner:1` are presentation only and are not spill or
execution input. The cache reader rejects malformed relation identities instead
of silently dropping them. Union equality uses the compiler's public schema,
excluding cache metadata while retaining declared underscore-prefixed columns.

These are deterministic runtime regressions, not evidence of an AppWorld score
or a replay of the entire failing evaluation conversation.
