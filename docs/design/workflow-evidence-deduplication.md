# Teaching reviews and filesystem archives

A learning review judges whether a captured **whole program** is a reasonable,
useful teaching example for its intent. Execution results are a separate concern:
the review does not own, copy, retain or depend on the full result artifact or
its provenance. The program is teaching material, not an executable reusable
workflow.

## What a review needs

Capture one immutable, scoped teaching observation per distinct authored program:

- The original Python source, purpose and compact compiler-rendered textual plan.
- The catalogue-qualified semantic references and contracts needed to understand
  and compile the program, including effects, predicates and ordering.
- A typed execution eligibility fact recorded by the host: which session and
  execution produced the observation, and whether it completed successfully.
  This is a small fact, not a copy of the result, collection or execution trace.
- A program identity and revision sufficient to bind the review to the exact
  source and semantic context. Credentials and session-specific values are never
  teaching defaults.

The review references those observations and records its semantic judgment.
Identical observations are retained once per authorized scope, including when
multiple runs or reviews refer to them. Typed references and content identities
protect integrity and scope; they do not require an execution-result blob to be
available later. The worker checks host-recorded eligibility and reviews the
program; it does not inspect, copy or replay provider results.

External judgments of session success are independent of both execution capture
and program review. Plasm accepts a generic, authenticated decision that a
session should be considered for review, with authority and assurance supplied
through the host's existing policy. Human review and delegated external systems
are possible sources of that decision; Plasm does not embed a grader or assume
how the external system established success. An external decision is not proof
that each program caused the outcome. Agent attestations and deterministically
established facts retain their distinct assurance.

The AppWorld agent owns its native task grader and collection policy. For that
agent, a fully passing grade is the proxy for an external success decision: only
then does it ask Plasm to consider the session for review. Failed, partial or
missing grades must not generate that request. Task IDs, grade formats and
grader logic belong to the AppWorld harness, not Plasm's evidence model or
generic publication rules.

Pages are downstream delivery and never learning observations. Run artifacts,
traces and results keep their own storage and retention policies. Expiring or
deleting a result blob does not invalidate a retained teaching review; a missing
program, its required semantic contract, the successful-execution fact, or an
external decision required by the host's policy does prevent admission.

## Compact semantic authority

Persist the semantic references needed to interpret and rebind the program,
rather than embedding full session ledgers, deployment inventories, raw binary
frames rendered as JSON numbers, or a copy of the entire catalogue per run.
Keep catalogue ownership and contract semantics intact. Shared immutable
contracts may be content-addressed within the authorized scope; a digest never
grants access. Ordinary compilation against the actual destination session is the
compatibility check before serving.

The captured teaching observation is sufficient for review and later teaching.
It need not reproduce the original session's complete symbol ledger or replay
the original execution. If the existing compiler cannot extract an adequate
semantic projection, retain the program as unpublished with a typed diagnostic;
do not reconstruct meanings by guessing symbol numbers.

## Sealing and cutover

Atomically seal the review's scoped observation references, semantic claims and
publication job. A review references exact immutable program revisions, and
retries are idempotent. Admission and publication retain their existing
authorization, quality, portfolio and concurrency checks. No operation replays
live effects to establish suitability.

The cutover starts with a clean learning store. Existing stores using the
previous evidence schema are rejected; no consent, authorization or
successful execution is inferred from their contents.

## Portable filesystem archive

The archive is a versioned directory, optionally packaged for transport:

```text
manifest.json                    # version, scope, roots, completeness and digest
records/                         # streamed programs, reviews and external decisions
objects/<digest-prefix>/<digest> # shared program/semantic-contract objects
```

Export a consistent scoped snapshot as bounded records and objects. Write each
distinct teaching observation and shared contract once. Do not aggregate the
entire corpus into one JSONB object or process buffer. The Plasm archive includes
programs, reviews, semantic dependencies and generic external decisions needed to
understand admission; it does not include execution result blobs or their full
provenance as a condition of teaching. AppWorld task assessments belong in the
harness's own archive, linked to the relevant generic decision where needed.
Optional separate execution archives remain independent products with independent
retention and disclosure rules.

Import checks the manifest, content digests, references, scope and limits before
transactional, idempotent adoption. Import does not grant authority, invent
consent, or turn an observation into a local execution. Current authorization,
catalogue compatibility and compilation remain serving checks.

## Verification requirements

- Repeated runs and reviews do not duplicate identical program observations or
  semantic contracts; result blobs do not appear in review or teaching archives.
- Review remains available after an independently retained execution result is
  deleted. Missing program semantics or a required eligibility fact fails closed.
- The AppWorld agent requests review only for fully passing attempts; its grader
  and grade format never become dependencies of Plasm. An external success
  decision is not proof of correctness of every program in the session.
- Scoped references reject cross-project access, changed program revisions,
  missing contracts, stale approvals and unsupported catalogue drift.
- Migration preserves external decision links and original seal identity without
  fabricating success; interrupted or missing data is visible for repair.
- Large exports and imports use bounded streaming and detect missing, truncated or
  corrupt objects before adoption. Normal discovery and execution continue if
  optional learning is unavailable.
