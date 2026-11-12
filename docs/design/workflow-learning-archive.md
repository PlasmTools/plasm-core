# Portable teaching archives

The normative evidence ownership and cutover requirements are in
[workflow-evidence-deduplication.md](workflow-evidence-deduplication.md).

A Plasm teaching archive transfers complete, reviewed program examples and the
minimal semantic contracts needed to understand and compile them. It does not
turn examples into reusable executable workflows, grant catalogue access or
authorize live effects. Execution results, pages, raw traces and task-specific
grader payloads are not teaching archive records.

## Ownership

The OSS learning ledger owns immutable scoped program observations, compact
execution facts, review decisions, example revisions and catalogue guidance.
Identical program observations are stored once within the authorized scope;
reviews and execution facts reference them. The normal Python compiler and
current catalogue authority determine whether an example may be served.

An external host owns its definition of session success. In the AppWorld
harness, the native grader runs after execution and before the agent requests
review; only a fully passing task can trigger that request. AppWorld task IDs,
grades, datasets and partition rules stay in the harness's assessment archive,
not the generic Plasm archive or core publication policy. A grade is not proof
that every program caused the outcome.

## Directory format

The archive is a versioned directory, optionally packed for transport. Its
manifest binds the archive kind, source scope, record identities and content
digests. Each bounded object is written once under its digest; records form a
typed, scoped reference graph. Private collections may retain compact historical
observations for repair. A serving-library projection must include only evidence
needed for its selected examples and guidance, never rejected sibling programs.

Export reads a consistent scoped snapshot in bounded pages and writes objects
incrementally. It must not construct a whole-corpus database JSON value or a
single whole-archive process buffer. A new destination is published atomically
without replacing an existing path. Import validates versions, sizes, digests,
reference closure, scope and authorization before transactional adoption.
Corrupt or unsupported material remains unavailable; missing objects do not
silently turn an archive into a complete one.

An archive digest establishes integrity, not authority. Historical source
identities must not be accepted as proof of execution merely because an archive
asserts them. Import authorization and any trusted external success decision are
separate actions. A delegated imported-review decision requires an explicit
project-scoped grant and remains subject to expiry, revocation and revision
checks. Imported material never becomes a local execution or manufactures
session consent.

## Cutover and verification

The new evidence schema starts with a clean learning store. Older stores are
rejected rather than converted implicitly. Archives produced by the new format
require explicit target-side authorization before their examples can publish.

Required checks include tampered or missing objects, cross-scope references,
duplicate and concurrent import/export, interrupted writes, no-overwrite
publication, symlink attacks, grant revocation, compiler and catalogue drift,
selected-only serving projection and re-export of imported support. Verify both
bounded memory and absence of raw results and host-specific grades. A read-only
preflight must open the ledger without running migrations or acquiring DDL locks.
