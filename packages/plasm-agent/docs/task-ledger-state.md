# Existing working-state retention

`PlasmAgent` supports `includeTaskLedger: true`. The product default is off.
The AppWorld worker now enables it unconditionally through its shared harness
options. Previously the worker required `PLASM_EVAL_TASK_LEDGER=1`, leaving the
ledger disabled when that experiment variable was absent.
It exposes `task_ledger` and appends the latest accepted record as a separately
labelled model-authored state message before each generation. The record is not
inserted into system instructions.

No new schema or production mechanism is required to represent restoration:

- `scope` holds the model's preservation constraints.
- `observed_facts` holds original-state observations and evidence references.
- `remaining_work` holds restoration and verification obligations.
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

This proves retention of an accepted model-authored obligation. It does not
prove that a model will record every obligation, retain it in a later accepted
whole-record replacement, restore state, or interpret verification correctly.
Evidence references are checked for syntax, not resolved as proof. The ledger
does not gate successful terminal calls or discharge obligations automatically.

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

The ledger guidance uses existing fields for preservation constraints, original
observations and unfinished restoration or verification. No domain-specific
procedure, new schema, execution budget or completion gate was introduced.
The product default remains off. No model evaluation was run.
