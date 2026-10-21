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

The store is instance-local memory, not durable checkpoint storage. It survives
tool calls and follow-up turns on the same agent. `resetConversation` clears it;
a new agent instance starts empty, even with the same `agentRoot`.

## Deterministic evidence

`npm run test:task-ledger` exercises an abstract resource with an original slot
and mode. A scripted model records its restoration obligation, invokes an
operation that fails, attempts an oversized ledger replacement, reads the
ledger, ends the turn, then receives a follow-up.

Assertions inspect the fresh state message in every subsequent provider request,
not merely the historical write. The original observation, preservation scope
and remaining obligation survive. The failed operation executes once. The
rejected replacement leaves state unchanged. Existing tests cover conversation
reset and the disabled configuration; the added test covers a new instance.

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
regex; validation feedback preserves the regex issue category and registered
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
