# AppWorld catalogue contract audit

Completed the ten-catalogue operation, input and response-contract audit against pinned AppWorld `0.2.0.dev0`. This is catalogue/runtime verification, not an evaluation score. Every one of the 469 OpenAPI operations has a mapping. This does **not** mean every mutation or possible backend state has been live-tested.

## Source and evidence

Backend image: `sha256:7eb56a352a4616dddaf9f617bdaf5286c8e62232ca077144e66983a668e7d018`. Independent OpenAPI: `/private/tmp/appworld-data-0.2.0/data/api_docs/openapi/`; backend implementations inspected in the pinned image. The disposable live probe loads task `18670a5_1` with `load_ground_truth=False`, uses real account identifiers and never evaluates a task score. Sources and live case inventory are retained here; session tokens are not.

## Catalogue repairs

- **Amazon:** all missing history, recommendations, purchase, last-purchase and rating-distribution operations; typed order lines, variants, rating buckets and receipt files; author identities and product counts/links; rating selectors. LastPurchase distinguishes requested product identity from the returned variant.
- **Gmail:** sender, typed recipients and attachments; full nested thread messages; status/snooze/label operations; mailbox/date/participant/status selectors and ordering; draft/reply/attachment inputs with file-system prerequisites. Removed fictitious thread subject. Typed datetime formatting encodes the documented wire layout only at transport.
- **FileSystem:** existing-directory, overwrite and date-retention controls.
- **Phone:** alarm owner phone number and name, checked against real backend output.
- **SimpleNote:** explicit pinned-note reorder control.
- **Splitwise:** creators, payers, receivers, receipt paths, expense shares and activity debtors; corrected activity compound identity; typed group-balance participants and outgoing/incoming debts instead of opaque nested JSON; balance totals and notification references. Added query/date/deletion/amount controls and receipt inputs. Removed fictitious User fields.
- **Spotify:** directory User search, review counts, album links, playlist owners, credited-artist relations, song membership, queue/current-player fields; collection filters and ordering; queue-position play and receipt outputs. Current-player state remains distinct from per-song state.
- **Todoist:** creators/assigners, labels, project collaborators/invitations/counts/status, sections/subtasks, notification references, filters and ordering, mutation options and file attachments. Section counts retain their vendor null-identity bucket as returned JSON; named sections remain typed relations.
- **Venmo:** transaction/comment liked state, notification references, history filters and sort, payment-request text search and receipt outputs. History-only filters are described as such.
- **Supervisor:** audited its six operations; no additional missing operation was found.

All ten catalogue versions were advanced. Descriptions and typed inputs remain domain-oriented; no task-specific answers or agent prompts were added.

## Compiler and runtime guarantees

1. Embedded primary IDs honor their declared nested wire path. Nonprimary shared compound identity slots are projected from **decoded parent fields**, with scalar type and value-format compatibility checked before compilation and when loading the compiled artifact.
2. Missing identity fields and empty identity paths are rejected. The child primary identity is never inherited merely because the parent has a same-named field. Request identities materialize with declared types, including numeric-looking strings.
3. The decoder codec rejects obsolete identity-hint fields instead of silently ignoring them. Property tests cover nested paths, inherited types, catalogue/decoder serialization, row order, duplicates and explicit empty versus omitted transitive relations.
4. Every Get requires a materialization sink. Parallel scoped Get, entity-reference traversal, relation hydration and query hydration retain the full decoded tree in owned branches and merge accepted observations. The previous optional-sink interface silently lost child rows.
5. Embedded-only target rows remain usable without a fictitious Get and retain Summary completeness. Abstract tests cover direct, scoped and query-hydrated entry paths.

These guarantees establish internal contract consistency and tested execution laws. They do not prove that arbitrary vendor responses satisfy the declared schema, nor that a human-authored semantic identity is correct. Independent source review and live probes remain necessary.

## Verification

| Check | Result |
|---|---:|
| OpenAPI operation mapping | 469/469, all ten catalogues |
| Hermit checks | 605 pass; 4 fail; 38 relation checks skipped |
| Compiler library, including codec/property tests | 73 pass |
| Runtime library (serial test execution) | 354 pass |
| Concurrency Shuttle tests | 12 pass |
| Abstract relation-only traversal test | 1 pass, three entry paths |
| Language relation matrix | 3 pass |
| Language view matrix | 14 pass |
| Real-catalogue focused integration tests | 4 pass |
| Actual pinned backend, explicit sequential transport | 20/20 probes pass |

The four Hermit failures are all Spotify Library composition: mock Album list/Get identities disagree (`Album:396` versus `Album:2`). They remain recorded as failures, not counted as passes. The same composed library traversal passed against the real pinned backend, returning 81 songs in 99 requests. The 38 standalone validator skips need parent context; focused and live tests cover representative paths, not every skipped relation.

The first broad live-library probe was invalid: direct `ExecutionConfig::default()` did not apply host environment overrides. Concurrent backend reads produced spurious 409/500 responses. The opt-in test now explicitly sets process, per-origin, hydration and backend concurrency to one. The final 20-case run is the sequential evidence. The invalid run is retained separately and is not catalogue failure evidence.

The broad runtime suite initially exposed an obsolete test expecting an undeclared `sess.access_token` reference to parse as a literal; it now requires rejection under the scalar-boundary contract. One tracing test failed during parallel suite execution and passed isolated and in the serial full suite; no runtime tracing behavior was changed. Existing dead-code/linker warnings remain.

## Wider compatibility checks

A schema sweep across all 53 catalogue directories passed 34, including all ten AppWorld
catalogues. Nineteen other catalogues stop at existing scalar-identity or unused-CML-input
validation rules, before the new embedded identity check. They are preserved in
`catalogue-schema-sweep.json`; this does not establish compatibility for those blocked
catalogues. The CLI suite passed its three validator tests; its separate ER-diagram test
fails because an older Account.contacts fixture has no required relation materialization.
That fixture was not changed in this audit. These residual failures prevent any claim
that the entire repository is green.

## Mechanical audit residuals and dispositions

`input-contract-residuals.txt` records the independent matching audit without suppressing exceptions:

- Login's optional OAuth form `client_id`, `client_secret`, `grant_type`, `scope` are unused framework fields rather than domain inputs.
- Phone window endpoints are false matches for the regex's `{message_id}` route; distinct window capabilities already cover their selectors/pagination.
- Artist.songs is an intentionally narrow exact-artist reader. The wider `/songs` selectors are available through Song's catalog collection.

`response-contract-residuals.json` is similarly preserved:

- ProductType.name, Gmail Label.name and DirectoryEntry.path originate in scalar-list members.
- ReturnDeliverer.deliverer_name is mapped from wire `name`; the checker does not interpret that string shorthand.
- PathExists.path and ProjectInvite.invite_code are request/receiver identities, not response properties.
- TaskCompletion.status is a control-entity slot; complete_task declares a side effect and does not claim to return a status row.
- Notification payloads have unspecified OpenAPI object properties. The optional typed IDs were verified against the pinned model constructors in Splitwise, Todoist and Venmo.

No remaining mechanical item is silently presented as a proven response field. Arbitrary notification payload additions, every optional mutation combination, and all possible backend states are outside the live evidence.

## Reproduction and cleanup

Run `target/debug/plasm-cgs validate plasm-oss/apis/appworld/<app> --spec <pinned-spec> --require-complete` for each app. The logs retain all failures/skips. The audit scripts are read-only review aids; they do not generate CGS.

The opt-in test is `cargo test -p plasm-e2e --test appworld_catalog_contracts pinned_backend_catalog_contracts -- --ignored --nocapture`, with `PLASM_CONTRACT_PROBE_MANIFEST` set to the private manifest produced by `live_probe_driver.py` in the disposable pinned worker. `LIVE-CASES.json` is a sanitized case inventory. The driver expects `/dataset` and `/proof` mounts, and exits when `/proof/stop` appears.

The owned probe worker was stopped, port 9000 was confirmed free, and its private manifest removed. No scored run, commit, dependency change or unrelated container cleanup was performed.

## Deployment integration follow-up

The challenge rerun preflight exposed a gap beyond direct catalogue execution: 22 new prerequisite requirements lacked entries in the separate deployment manifest. Added explicit same-service session and FileSystem-session bindings; the manifest now covers exactly all 435 declared requirements, without missing or unused entries. This was caught before any scored task admission. Direct HTTP probes alone had not exercised federated discovery deployment closure.
