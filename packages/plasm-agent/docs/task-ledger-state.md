# Existing working-state retention

`PlasmAgent` supports `includeTaskLedger: true`. The product default is off.
The AppWorld worker now enables it unconditionally through its shared harness
options. Previously the worker required `PLASM_EVAL_TASK_LEDGER=1`, leaving the
ledger disabled when that experiment variable was absent.
It exposes `task_ledger` and appends the latest accepted record as a separately
labelled model-authored state message before each generation. The record is not
inserted into system instructions.

The existing record separates preservation from intermediate work:

- `scope` holds the model's preservation constraints.
- `observed_facts` holds original-state observations and evidence references.
- `obligations` holds stable-ID pending, deferred, resolved or cancelled work.
- `completed_effects` holds model interpretations of completed work.

When enabled, the store checkpoints accepted records to `agentRoot/task-ledger.json`
(or explicit `taskLedgerPath`). AppWorld supplies its task workspace path. Use one
writer and one workflow per checkpoint; this is not a shared multi-agent store.
Writes validate the record, flush a mode-0600 temporary file, and atomically rename
it before acknowledging or updating memory. A new instance loads the versioned
record before any model request. Corrupt or unsupported records fail with a typed
load error; they are never silently replaced with empty state. `resetConversation`
explicitly persists an empty record. Failed writes leave accepted memory unchanged.
The bounded 4 KiB checkpoint uses synchronous file operations at the write boundary.
Loading a ledger restores model-authored working state only: it does not resume a
conversation, restore execution authorizations, or replay any operation.

## Deterministic evidence

`npm run test:task-ledger` exercises an abstract resource with an original slot
and mode. A scripted model records its restoration obligation, invokes an
operation that fails, attempts an oversized ledger replacement, reads the
ledger, ends the turn, then receives a follow-up.

Assertions inspect the fresh state message in every subsequent provider request,
not merely the historical write. The original observation, preservation scope
and remaining obligation survive. The failed operation executes once. The
rejected replacement leaves state unchanged. Existing tests cover conversation
reset and the disabled configuration. Restart tests cover obligation and completed
effect retention, fresh model-request visibility, no effect replay, file permissions,
failed atomic replacement, durable reset and corrupt-checkpoint rejection.

## Limits

This proves retention of accepted model-authored obligations. It does not prove
that the model records every obligation or restores state. General fact and
effect references remain syntax-checked claims. Obligation resolution has the
separate artifact check below. The ledger does not gate terminal calls or
discharge obligations automatically.

Evidence syntax is declared in the registered JSON Schema: a bare `pr` plus
64 lowercase hexadecimal digits, `pc` or `pg` plus digits, a nonempty
whitespace-free `plasm://` URI, or a supported Plasm tool name. Labels and
surrounding whitespace are rejected. Admission and teaching share the same
portable regex. Host admission also requires the match to equal the complete
input, rejecting JavaScript's special final-line-terminator `$` match. Provider
schemas contain no lookaround; abstract tests compare admission against the prior
absolute-end grammar across reference kinds and trailing line terminators.
Validation feedback preserves the regex issue category and registered
pattern/description without copying rejected argument values into instructions.
The abstract SDK recovery test verifies rejection, an admitted replacement
executing once, retained obligations and immutable rejected-call audit data.

The installed-source AppWorld instruction proof uses the same harness options
as `run-task.ts`. A scripted model writes an abstract restoration obligation and
then calls `complete_task`. The proof checks the exact fresh ledger message in
the provider request that produces completion, confirms the domain-neutral
guidance is present, and verifies that terminal execution leaves the record
intact. This deliberately proves visibility, not enforcement: the scripted model
can still claim completion with outstanding work.

## Bound verification

An open obligation may bind `verification` to an existing review using
`logical_session_ref`, `plan_commit_ref`, `binding` and `field`. This identifies
a model-authored compute predicate, not another natural-language assertion.
Recording pending or deferred work requires no verifier or archive lookup.
If an optional, well-formed binding is unavailable or inadmissible, the host
stores the open work while omitting that binding (or retaining its previous
accepted binding). The acknowledgement names the obligation and the rejected
binding, and states whether the old binding remains. The same ID can attach a
valid binding later without recreating the work. Malformed updates and invalid
resolutions still reject the whole update; no status is silently downgraded.
The predicate persists when omitted and cannot change with wording refinements.
A fresh review reference is admitted only when both archived plans have identical
program, computation, catalogue, workflow, producer binding and field. This permits
fresh observation after a consumed authorization without substituting the predicate.
Resolution cites a `pr` artifact from that review and workflow. Archive receipts
must identify the selected producer and a complete single computed row whose
named field is exactly boolean `true`. Missing, null and false are distinct.

The current boundary accepts straight-line read-only verification plans. The
computation must have a value-input path to an observed read; ordering edges,
constants and historical outputs alone are insufficient. Historical outputs may
still supply an original-state baseline alongside a read. Writes, nested plans
and unknown node forms are rejected rather than inferred. Checks read existing
archives only; they never execute, replay, or fetch provider data.

The model remains responsible for the predicate's meaning. A declared read
dependency does not prove that Python uses that input correctly, and a result
describes its observation time, not all future state. No natural-language
preservation predicate is inferred, and later writes do not automatically reopen
resolved obligations. Explicit cancellation remains distinct from resolution.
Verification needs available plan/run archives and an active workflow session.
Every execution attempt advances the durable workflow observation revision.
Resolution rejects evidence preceding or overlapping another execution, including
failed attempts. All revision witnesses are rechecked together before accepting
the ledger update. This is resolution-time freshness, not a completion gate.

Ledger checkpoints are now version 3. Older checkpoints fail explicitly; no
unverified historical resolution is silently promoted. Abstract tests cover
bound predicates, atomic rejection, concurrent updates, and restart retention:
`node scripts/run-plasm-cli.mjs scripts/test-obligation-evidence.ts`.
No system instructions, catalogue recipes, budgets, dependencies or evaluation
runs changed. The tool description replaces its old resolution claim to match
the stricter contract; the product default remains off.
