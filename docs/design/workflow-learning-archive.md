# Training collections and portable teaching archives

Status: implemented and verified offline on 2026-11-02. The OSS archive domain, repository
adoption, native API and host CLI implement this boundary. Existing PostgreSQL
snapshots remain experiment backups, not the portable format.

## Purpose

Collect complete authored programs on a frozen training partition, retain their
execution and assessment evidence, and carry a fixed teaching library into later
evaluation ladders. Programs teach authoring; importing one does not authorize
execution or establish formal reusable-program equivalence.

The collection must retain useful observations even when review or promotion does
not finish. Archival retention and serving eligibility are separate decisions.

## Collection boundary

Freeze task IDs and group IDs, dataset identity, source revisions, worker image,
catalogue generation and deployment bindings, model settings, solve/review budgets,
grading version, promotion policy, and stop rules before dispatch.

The user selected all 90 AppWorld train tasks on 2026-11-02, with evaluation on
separate test ladders. The earlier 24/12/12/42 partition remains historical
provenance only. Its development and reserved train groups all become training
data for this new collection; they cannot support subsequent holdout claims.
Register a new train-only partition rather than editing the historical manifest.

Native grading runs after execution and remains unavailable to the reviewing
agent. Record a task assessment against its attempt, not against each program as
though the grader supplied causal attribution. Program verification, evidence
assurance, task outcome and measured robustness remain distinct typed dimensions.

Before collection, validate evidence inspection, submission, decline and budget
exhaustion with offline reviewer fixtures. These establish protocol behavior, not
live model reliability. The previous two-generation,
4,096-output-token, 120-second protocol did not submit any review in three started
phases. Do not launch a larger collection under unchanged assumptions or fabricate
reviews from native task grades. Any revised reviewer policy is a new frozen
exploratory protocol, not a revision of the historical run.

## Archive contents

Use a versioned, deterministic JSON format with existing serde and digest support;
add no dependency. Bound document bytes, item counts and individual source/evidence
sizes before admission. The format is independent of PostgreSQL table layout.

An archive contains two explicitly distinguished products:

1. Observation records retain whole authored source, original compiler/symbol and
   deployment authority, run provenance, execution evidence, review disposition,
   and separately attributed task assessments. Unreviewed, rejected, superseded
   and incompatible observations remain inspectable and are not serving entries.
2. Library records retain eligible teaching revisions and independent catalogue
   guidance, normalized intent/contracts, exact supporting evidence identities,
   quality dimensions, compatibility versions and publication provenance.

The manifest identifies the format version, archive digest, source scope,
collection protocol, task/group membership and completeness of referenced evidence.
Typed records retain catalogue contracts, compiler/teaching versions and
normalization/embedding profiles without duplicating them in a second inventory.
Canonical item digests provide integrity and idempotence; they are not signatures
or evidence that the exporter is trusted.

The current bounds are 512 MiB per document, 10,000 items and 16 MiB per item.
Exports reject overflow rather than truncating the corpus. Task membership must
match retained assessment/protocol records; supporting sessions without known task
attribution remain explicit and cannot be adopted as serving products.

Raw observations can contain private task/runtime values. Keep their export
explicitly private. A serving-library export includes only disclosure-eligible
teaching source and its required provenance, with external evidence references
marked as such. Never silently drop evidence while describing an archive as
self-contained. Credentials, connection URLs, access tokens and live execution
handles are not portable configuration and must not be imported as defaults.

## Typed ownership and interfaces

Implement the archive domain in OSS alongside workflow learning. Reuse validated
session/run/program IDs, scoped repository transactions, teaching validation,
portfolio admission and catalogue resolution. The AppWorld harness owns task-grade
attachments and partition registration; it does not implement a second publisher.

Use explicit types for archive identity/version, observation identity, assessment
scope, evidence completeness, import provenance and import disposition. Examples
of distinct dispositions are retained observation, duplicate, awaiting compatibility,
eligible candidate, quarantined and rejected. Do not encode these as booleans or
an optional error string.

Keep byte decoding/integrity validation separate from repository adoption and
catalogue compatibility. Repository export observes one consistent scoped
snapshot. Repository import is transactional and idempotent by archive/item digest;
failure must not leave a partly adopted library. Use existing repository and
catalogue interfaces rather than introducing traits without independent behavior.

## Import and local adoption

The importing host supplies the target tenant/project through its authorized
boundary. The archive cannot grant access or select a privileged target scope.
Preserve original source scope, IDs, seals and digests verbatim in provenance;
record target-scope adoption separately instead of rewriting sealed evidence.

Import does not install source approvals, execution receipts as local executions,
jobs, credentials, session ledgers or queued effects into a live session. Archived
symbols are retained interpretation evidence; fetching binds semantic references
through the destination's actual session ledger.

Validate supported formats, referential integrity, evidence completeness, source
disclosure, catalogue/deployment contracts and provenance policy before local
admission. Imported examples and guidance enter existing bounded publication
rules; no direct insertion into active portfolios. Exact duplicates add provenance
without growing the active library. Incompatible versions remain quarantined with
diagnostics; they are not silently upgraded or discarded.

Normal production compilation remains the executable drift check. Exact supported
catalogue/compiler compatibility identities may reuse appropriate cached checks;
they do not bypass actual-session binding or current authorization. Missing
connections/permissions are request eligibility outcomes, not global evidence that
an example is corrupt. Infrastructure failures remain unverified.

## Evaluation consumption

Freeze an archive identity before evaluation begins. Each ladder arm imports into
an isolated target library and records the archive digest and import result.
Evaluation runs cannot promote their own sessions into that library. Request-time
quarantine or compatibility observations belong to an arm-local overlay and do not
mutate the source archive or another arm.

Reject intersections with evaluation task/group membership where identities are
available. Report literal generalization separately from unseen groups and disclose
unknown cross-dataset overlap; hashing task text alone is not a semantic leakage
proof. Keep native grades and reference solutions outside agent retrieval.

## Verification gates

- A failed or unsubmitted review still yields an archived scored observation, but
  never an active example merely because the containing task passed.
- Deterministic export/import/export preserves item identities and provenance.
- Reimport is idempotent; conflicting duplicate identities are rejected atomically.
- Missing objects, corrupt hashes, unsupported formats and oversized records fail
  before publication; incompatible retained data cannot serve accidentally.
- Source-scope seals remain unchanged across authorized target adoption, and
  unauthorized cross-tenant import/disclosure fails.
- Changed deployment bindings and same-type semantic catalogue drift require
  compatibility review; actual-session compile failures quarantine serving revisions.
- Imported source never executes during validation; effects require ordinary
  plan/review/run.
- Baseline, guidance-only and optional-example arms consume independently frozen
  libraries, with all outcomes and archive identities reported.

## Delivery order

The pre-collection gate passed strict Clippy for the affected Rust targets,
73 workflow tests against PostgreSQL, eight symbol-context tests, and all 41
native tests. The older optional native subprocess proof was explicitly excluded;
the new real CLI proof separately checked cross-process export/import, duplicate
import, byte-identical retained export, and absence of fabricated target reviews
or execution receipts. Reviewer and manifest TypeScript proofs and 47 Python
protocol tests passed. These checks establish implementation behavior; they do
not establish retrieval usefulness or live reviewer reliability.

1. Freeze the approved all-90 training scope and revised bounded review policy.
2. Prove review submission/decline and observation retention with offline fixtures;
   measure live reviewer reliability during the registered collection.
3. Implement and verify OSS archive round-trip plus authorized local adoption.
4. Run the selected training collection with the corrected committed native engine.
5. Drain, inspect and publish eligible products; export both private observations
   and the fixed serving library with complete accounting.
6. Import the serving archive into independent evaluation ladders. Measure outcome
   success, wrong-example rate, catalogue recall, turns, teaching tokens, latency
   and library growth without learning from evaluation outcomes.
