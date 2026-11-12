# OpenAPI and semantic-description audit — 2026-09-17

No AppWorld task replay or evaluation was run. No live service writes were made.

## Repairs

- Added 82 missing operation mappings across seven AppWorld catalogs: 54 account lifecycle operations and 28 comment/review/card/status/project operations.
- Registration, verification and recovery are separate domain actions. Session-requiring operations carry login prerequisites; registration does not assume an existing account.
- Preserved comment/review authors and phone message participants on newly exercised reads.
- Preserved Todoist invitation codes only on the invitation operation, not ordinary project reads.
- Replaced storage/transport-oriented identity wording. File operations select by full path; the numeric file reference is described separately. Fixed a malformed Venmo sort-description YAML value.
- Scanned descriptions in all 53 catalog files for storage/transport terminology. Applied description repairs to nine AppWorld catalogs and seven other catalogs. Domain concepts such as Notion databases and SQL queries were retained. This lexical/semantic pass is not proof that every field in every catalog is complete.

## External operation baseline

The existing `plasm-cgs validate --spec ... --require-complete` command now rejects missing operation mappings before Hermit runs. All 453 operations in the ten pinned AppWorld specifications have mapping coverage. The gate handles conditional/format paths and finite enum selectors; it does not mistake an arbitrary identity parameter for a named static endpoint. It is a necessary structural condition, not proof of branch reachability, full response-field coverage, or live behavior.

## Verification

- `cargo test -p plasm-core --lib temporal::tests`: 24 passed. Absolute Date inputs and frozen clocks now share ISO/RFC parsing; timezone-naive fractional timestamps mean UTC and preserve nanosecond precision. Invalid dates and relative clock anchors remain errors.
- `cargo test -p plasm-compile --lib openapi_pagination`: 4 passed, including abstract operation-coverage tests.
- `cargo build -p plasm-cli --bin plasm-cgs`: passed.
- Schema validation: all ten AppWorld catalogs pass.
- Hermit results below. Todoist's final rerun passed 74/0/0 with one skip after the invitation output correction; the retained intermediate log records the failure that prompted that correction.

| Catalog | OpenAPI operations | Unmapped | Hermit pass | Fail | Skip |
|---|---:|---:|---:|---:|---:|
| amazon | 66 | 0 | 91 | 0 | 2 |
| file_system | 26 | 0 | 30 | 0 | 0 |
| gmail | 42 | 0 | 44 | 0 | 2 |
| phone | 30 | 0 | 38 | 0 | 0 |
| simple_note | 17 | 0 | 20 | 0 | 0 |
| splitwise | 65 | 0 | 85 | 0 | 2 |
| spotify | 91 | 0 | 114 | 4 | 5 |
| supervisor | 6 | 0 | 6 | 1 | 0 |
| todoist | 56 | 0 | 74 | 0 | 1 |
| venmo | 54 | 0 | 66 | 1 | 0 |

## Remaining validation limits

Six Hermit checks fail: four Spotify Library checks encounter inconsistent generated list/detail identities; Venmo Friend and Supervisor AccountPassword derived Gets search generated collections for the invented `test-1` identifier. These do not establish a live runtime defect, and they are not counted as passes. Twelve parent-context relation checks remain skipped. New mapping correctness must not be inferred merely from the operation inventory.

Outside AppWorld, description-only edits exposed existing structural validation failures: GitHub declares unused parameters on `user_repos_query`; Gmail, Google Drive and Discord use entity-reference fields in compound identities rejected by the current compiler; Hacker News declares pagination inputs without a driver. Fibery and Rick and Morty schema validation pass. These structural defects were not altered by this description review.

NAPI and the evaluation container were not rebuilt. No benchmark improvement is claimed.
