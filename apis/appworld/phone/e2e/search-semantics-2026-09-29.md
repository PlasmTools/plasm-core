# Message search semantics

The pinned AppWorld worker `plasm-appworld:rowset-smoke-20260928t231556z`
implements text and voice message search in `appworld/apps/phone/apis.py`.
Both endpoints restrict the conversation by `phone_number`, optionally select
the newest message per contact, then invoke `retrieve_dicts(query=...)`.
The shared ORM defaults `include_non_matching=True`: matching results rank first,
with nonmatches retained. Exhaustive pagination does not establish text membership.

Phone catalog version 19 teaches these distinctions on the corresponding input
slots for both TextMessage and VoiceMessage. No transport, selection behavior,
result cap, or task-specific strategy changes. Descriptions are served by the
normal CGS card renderer to every consumer, not injected by the AppWorld harness.

Pinned ORM verification uses three synthetic records in SQLite FTS:

| Search | Default result IDs | Diagnostic include_non_matching=False |
|---|---|---|
| needle | 1, 2, 3 | 1 |
| nonexistent | 1, 2, 3 | none |

All four assertions passed on 2026-09-29. The diagnostic false setting is not
an exposed Phone endpoint parameter and is not taught as one.

Parent-workspace evidence: `target/search-teaching-repair/` and
`target/search-hydration-diagnosis/search.py`. This is catalog integration evidence,
not a new language-matrix obligation or an agent task result.

Current-source catalog packer passed CGS, CML, view and publication validation.
The pinned production NAPI engine loaded that publication and rendered both
message cards; assertions confirmed all three semantic comments appear on each.
A Python Program copied from the TextMessage query signature compiled and ran
against a controlled transport: `query=needle` and `phone_number=1234567890`
arrived intact, one complete result decoded, and no detail request was needed.
Evidence scripts: `target/search-teaching-repair/verify.cjs`, `pack.log`,
and `teaching.txt` in the parent workspace. The pre-existing local CLI was stale;
it rejected the existing datetime profile, so the packer was rebuilt from source.

Broader Hermit and live task tiers were not rerun for this description-only
repair; mappings and execution semantics are unchanged. No agent evaluation was
performed. The ORM check supplies backend semantic evidence that schema-generated
mock responses cannot establish.
