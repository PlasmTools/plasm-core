# Inner Python profile: Monty typed-v11

This ledger supplements the normative [compute contract](python-compute-contract.md).
It describes inner `@compute`, not the declarative grammar of `Program.build`.

The vendored fork's upstream base is revision
`e007685fbb06494c13b9a7b3fede9f8e6a54a2be`. Its checker includes the documented
structured-analysis export extension; interpreter and typeshed semantics remain
those of the upstream base. The reviewed Python node carries
contract version 11 and profile `monty-e007685fbb06494c13b9a7b3fede9f8e6a54a2be-typed-v11-money-v2-branches-v2`.
Deployment must build host and worker from the same vendored checkout. Changing
interpreter, checker or typeshed semantics requires a profile upgrade.

## Classification and evidence

The upstream [language inventory](https://github.com/pydantic/monty/blob/e007685fbb06494c13b9a7b3fede9f8e6a54a2be/docs/limitations/language.md)
is the execution authority. Plasm does not copy its parser into a whitelist.
Representative tests are `python_compute/upstream.rs`,
`python_compute/profile_tests.rs`, `python_compute/schema.rs`,
`python_pool/tests.rs`, and the Python language matrix.

| Family | Classification | Witness |
| --- | --- | --- |
| Constants, names, attribute access, arithmetic, unary/binary/boolean/comparison operators | Upstream checked; boundary names are explicit | Operator witness and primitive matrix |
| Conditional expressions, null narrowing, membership | Upstream checked | Nullable/branch test; minimal CSV matrix |
| Assignment, annotated/augmented assignment, unpacking, named expressions | Upstream checked | Assignment and walrus witnesses |
| `if`, `for`, `while`, `break`, `continue`, loop `else`, `pass`, `return` | Upstream checked; execution bounded by pool | Loop witnesses |
| Lists, tuples, sets, dictionaries, slicing/subscripts | Upstream checked | Collection and subscript witnesses |
| Comprehensions and generator expressions | Upstream checked; generator expressions materialize lists in this pin | Comprehension witness; upstream documented divergence |
| Calls, local functions, closures, lambdas | Upstream checked; no implicit access to another Program helper | Closure witness; unknown-name rejection |
| Local mutation | Permitted on materialized copies; cannot mutate the session graph | Mutation witness and runtime boundary |
| Strings, multiline/raw strings, f-strings, formatting, string methods | Upstream checked | Formatting witness and rendering matrix |
| `try`/`except`/`else`/`finally`, `raise`, `assert` | Upstream checked; uncaught exceptions fail compute | Exception witnesses and pool error tests |
| Simple local classes, nested decorators, synchronous context managers | Upstream checked | Local class/context-manager and nested-decorator round trips |
| Inner async functions/await | Nested definitions are checked and compiled; root compute remains synchronous | Nested async-definition witness; no async host callables or coroutine return contract |
| Yield/yield-from, match, del, exception groups, PEP 695 aliases, complex constants, class inheritance | Upstream unsupported | Pre-execution rejection witnesses |
| Async with/for/comprehensions, template strings, method decorators | Upstream unsupported | Pre-execution rejection witnesses |
| datetime imports | Allowed, including aliases; typed temporal IO | datetime matrix and codec tests |
| Imports, global/nonlocal and lexical bindings | Upstream checked and compiled | Upstream scope and module witnesses |
| Reflection and builtin availability | Upstream checked and compiled; names grant no host authority | Boundary and undeclared-interaction witnesses |

The inventory classifies syntax families, not every possible Python program.
The witnesses establish representative behavior within the pinned profile. New
evidence updates this ledger without adding local expression inference. Syntax that the upstream checker accepts but Monty cannot compile is
rejected when the definition is compiled, before any DAG effects.

## Modules and host facilities

Module and member availability belongs to the pinned Monty checker and compiler.
Plasm has no module, builtin-name or dunder-name whitelist. Arbitrary installed
Python libraries are not available. Host-generated checker declarations are
separate from runtime imports and capabilities.

Pure compute receives materialized inputs and declared exact-money operations,
not mounts, catalog methods or credentials. Undeclared host/OS interactions fail
at the suspension boundary. Ambient random initialization and sleep request the
host and are rejected; explicitly seeded local randomness needs no host authority.
The existing configured clock policy governs datetime. Print output is discarded.
Local names such as `open` grant no filesystem access. Unknown callables and
unsupported dynamic operations fail upstream admission or compilation.

The checker accepts `bool` as an `int` argument, while this pinned interpreter
rejects direct `bool + int` arithmetic at runtime. Plasm preserves the Boolean
input and reports that upstream runtime failure; it does not insert a coercion or
reinstate a contradictory local subtype rule.

## Boundary and operational rules

The whole source schema is materialized. Use `.select(...)` before compute to
reduce it. A catalog annotation cannot restore projected-away fields. Missing
observations retain absent attributes in observed-record contracts; accessing one
raises `AttributeError`, which compute can catch with `try`/`except`. Explicit
projection and closed records still require their keys. Explicit nullable values
remain `None`. `hasattr` is supported by both the pinned checker and runtime. Arrays, sets, nested records,
unions, enum tokens and semantic domain identities retain recursive contracts.
Records use attributes/subscripts; dict/list[dict] compute inputs expose typed mapping views with `.get()`. JSON dictionaries use subscripts. Invalid
Python attribute identifiers fail explicitly. JSON has a recursive union type,
not `Any`; dynamic JSON operations require upstream narrowing.

Every compute requires a typed DAG callsite. Constant defaults use upstream argument binding;
only the root `@compute` decorator and admitted boundary annotations are allowed.
Admission rebuilds a definition using generated annotations, and never invokes
its body. Nested definitions execute only when the outer compute runs.

Admission runs the upstream checker and compile-only Monty frontend in-process.
The analysis request bounds source plus stubs to 256 KiB, AST depth to 128 and
exported type references to 16,384. Definition checking requests no exported
roots. The server schedules at most four concurrent checking jobs; cancellation
discards results while an already-running job retains its slot until completion.
These are compiler admission/concurrency bounds, not subprocess memory or time limits.
Execution retains 100 ms feed/turn limits, 16 MiB sandbox memory, 32 recursion
depth, 256 input rows, 1 MiB input and 1 MiB combined encoded value output. Queueing has a
five-second checkout deadline. Errors/cancellation discard the worker; successful
checkouts reset it before reuse. Static checking or compilation failure prevents review; execution worker failures remain runtime failures.

Return annotations resolve recursive Plasm value contracts, including named
domains, records, arrays and unions. Runtime validation precedes publication and
dependent effects; see the [typed return contract](python-return-contract.md).
