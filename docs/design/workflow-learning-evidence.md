# Workflow learning evidence and publication

The evidence and archive cutover is specified in
[Teaching reviews and filesystem archives](workflow-evidence-deduplication.md)
and [Portable teaching archives](workflow-learning-archive.md). This document
summarizes the execution and review boundary.

## Capture and review

Training is opt-in and prospective. When an authorized training execution
starts, the host records an attempt in the session journal. On successful,
complete execution the host records one scoped execution fact with its own
`we…` execution identity and a reference to the complete authored program.
A result artifact's `pr…` identity refers to separately retained result
delivery; an execution fact does not require a result artifact.

A review must explicitly cite eligible completed execution identities. MCP
clients can list them with `plasm_training` action `executions`; HTTP clients
can list them under the session's training `/executions` resource. The host
seals the chosen references and queues review atomically. Disconnects, timeouts,
pages and unsuccessful or unresolved executions do not produce successful
teaching evidence. Review statements remain agent-attested unless a separate
human or deterministic authority establishes the stated outcome.

Identical program observations share storage within their authorized project.
The observation retains the whole source, textual plan and semantic catalogue
references needed for teaching; it does not duplicate result blobs, full traces,
session symbol ledgers or provider output. Retaining a fact does not itself
approve a candidate.

## Publication and serving

Review evaluates whole programs for their teaching value. Deterministic gates
own evidence, authorization, compatibility and bounded portfolio decisions.
Human review is the default; automatic publication requires an explicit
delegation. Approval binds the exact candidate revision and supported guidance.
A changed candidate requires a new decision.

Discovery may offer up to three relevant examples after normal catalogue
authorization. Optional retrieval rebinds their symbols and compiles against the
actual session. Examples teach an agent; they are not automatically executed or
reused as workflows. Incompatible revisions remain stored for repair.

## External outcomes

An external system decides whether a session should be submitted for review.
In AppWorld, the harness runs its native grader and submits only fully passing
sessions. The grader, task IDs and grade payloads do not enter the generic
Plasm evidence or archive model. A passing session does not by itself prove
that every program contributed to its outcome.

## Cutover

The new evidence schema starts with a clean learning store. Existing stores
using the previous schema are rejected; the cutover does not convert or
implicitly authorize historical material.
