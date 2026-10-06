# Current-resource identity and scheduling repair

## Changes

Amazon Cart no longer identifies itself by the changing total_cost. It uses the existing typed implicit request identity contract, scoped by access_token, matching the established account-scoped read model. This is request-scope identity, not a claim that credentials are a durable account identifier. The total remains returned data. No compiler/runtime escape or new dependency was added.

Gmail Draft now states that scheduled_send_at triggers automatic delivery and that manual send dispatches immediately and removes the draft. Capability, effect and field/input descriptions retain these distinctions. Evidence: pinned AppWorld standard Gmail create_draft and send_email_from_draft documentation, plus the completed challenge trace.

Authoring reference now prohibits using changing observations as invented identities and requires preservation of automatic/manual and immediate/scheduled effects.

## Verification

- Amazon and Gmail schema validation: passed. Existing YAML helper-anchor warnings remain.
- cargo test -p plasm-e2e --test appworld_catalog_contracts: 5 passed, 1 ignored (requires disposable pinned backend and private manifest).
- New catalogue integration test exercises parsing, CGS JSON roundtrip, compiled artifact codec, real local HTTP and decoding. Same scope preserves identity when total changes from 23.40 to 0; a different request scope has a distinct identity. Auth header is checked.
- Existing Gmail temporal transport test passes.
- Actual native selected-capability teaching rendered and inspected: CURRENT-READERS-TEACHING.txt shows Cart request-scope braces and automatic/immediate Draft descriptions. Temporary rendering probe removed.
- No new pinned-backend run or model evaluation: these checks establish catalogue/mock correctness, not an improved task score.

## Wider audit findings still open

A static path is not sufficient to identify a singleton: File and public Profile readers put their keys in query parameters; derived friend/password readers have no direct path. Do not mechanically rewrite them.

Other current-read candidates require their own semantic repairs: Amazon PrimePlan uses monthly price, Spotify PremiumPlan uses monthly price, Phone DateTime uses current date, Gmail CategorySizes uses inbox count. PrimePlan and DateTime are request-scoped; PremiumPlan is public; CategorySizes also supports a read/unread selector. These are not all the same identity contract. This pass repairs Cart only, rather than silently collapsing selected snapshots or fabricating public credentials.

## Retrieval evidence

The completed run now contains triage/CANDIDATE-COVERAGE.json, reconstructed from actual successful Jev packets and answers. Camping: 64 candidate presentations across 8 packets, 47 distinct candidates, 17 repeated presentations. Face shield: 40 across 5, 25 distinct, 15 repeated. Prime sharing: 24 across 3, 20 distinct, 4 repeated.

Repeated presentation is a measurement, not proof that all repetition is waste: current needs can change and relevant operations can recur. The decisive defect is that explicitly requested purchasing operations are missing from early packets. They are accepted when finally presented after narrowing the request. The route has two narrowing stages: fused recall32 and rerank8. Archived Jev packets prove omission before judgement, but do not identify which earlier stage lost each operation. No ranking-policy change is justified by this evidence alone. Next offline ladder must preserve both candidate stages and score required-operation coverage at each, with fixed budgets and unchanged original intent; no whole-catalogue Jev expansion.

Follow-up: exact cached stage replay is now available in the completed run under triage/retrieval-stages/REPORT.md. It resolves the earlier attribution limit: five labelled losses occur at fusion32 and ten at rerank8, across 31 explicit-operation diagnostic probes. All 16 archived shortlists were reproduced without provider calls.
