# Scoped Python callbacks

A callback is a lexical, statically elaborated function body over a typed row.
A declaration without defaults emits no executable nodes. Defaults bind dependencies
at the declaration site; no Python source is executed during planning. Instantiating it at a
rowset consumer produces a reviewed scoped DAG. Monty remains responsible for
Python call binding, expressions, annotation compatibility, reachability and lexical scope evidence; the
frontend maps that evidence onto DAG bindings and control flow. Callback source is never executed by the compiler.

## Laws

| Law | Obligation |
| --- | --- |
| CB-01 Declaration | A local `def` creates a lexical callable version. Defaults capture declaration-site dependencies; earlier captures retain their version. |
| CB-02 Lexical scope | Free references resolve in the definition's lexical environment. Parameters and local bindings cannot accidentally capture caller locals. |
| CB-03 Invocation | A consumer invokes the body once per admitted parent occurrence, in the same bounded scope as its lambda form. Duplicate occurrences remain distinct. |
| CB-04 Sequencing | Each local binding is evaluated once. Effects retain program order; failure stops subsequent effects and parent occurrences. |
| CB-05 Branching | The condition is evaluated once. Both branches are reviewed, but only the selected branch executes. Early return prevents continuation on that path. |
| CB-06 Results | The consumer checks the inferred return contract: records for map, Python truth conversion for predicates, rows/effects for flat_map. A nonempty list of effect acknowledgements in flat_map is an ordered effect sequence, never value rows; mixing effects and values is rejected. Returning None contributes no rows; effects already executed on that path remain. Unselected branches dispatch no effects. |
| CB-07 Authority | Captures preserve cardinality, domain pins, coverage, and entity authority. A callback cannot manufacture receiver authority. |
| CB-08 Closure | Named callbacks and lambdas share lowering at each callback-consuming boundary. Recursion, escaping function values, and unsupported statements fail before IO. |

Consumers supply one positional row. The upstream Python call binder determines
which parameter receives it and whether remaining parameters have defaults.
Positional-only parameters and positional/keyword-only defaults are accepted.
Parameter and return annotations are checked upstream; return checking precedes
predicate truth conversion. Names are lexical declarations, not
runtime Python objects or new catalog capabilities. Assignments create immutable DAG versions, as in `build`; reassignment
publishes a new version without mutating previously captured dependencies. Higher-order function return values and recursion
are outside the bounded DAG contract; they must not silently become Monty host
calls.

## Implementation and witnesses

The same immutable statement lowerer serves `build` and named callbacks. `if`
control flow becomes two complementary zero-or-one-row scopes. A single Boolean
condition feeds both gates. Early returns terminate their path; the remaining
statements become the continuation of paths that have not returned. A record
join asserts exactly one resulting row at runtime. Effect acknowledgements come
from the complete executed scope ledger, including effects before `return None`.

| Boundary | Witness |
| --- | --- |
| Declaration erasure and alpha-renaming | `callbacks_declaration_erasure_and_alpha_renaming_preserve_semantics` compares canonical semantic plans across nine renamings |
| Declaration, captures, local records | `callback_lexical_record`, `callback_lexical_capture`; build constructor registry |
| Conditional records and predicates | `callback_branch_record`, `callback_branch_predicate` |
| Projection and iteration consumers | `callback_projection`, `callback_iteration` |
| Selected effects, None, early return, empty input, order and failure | `callbacks_live_conditional_effects`, independent HTTP dispatch log and completed-effect receipt counts |
| Recursion, unbound locals, predicate/projection effects | `callbacks_reject_unbounded_or_unbound_forms` |
| Existing lambda behavior | `callbacks_preserve_existing_lambda_contracts` |

`callbacks_live_lexical_record` executes the registered value witnesses with
independent expected results. The callback tests use abstract matrix catalogs;
no AppWorld-specific behavior is introduced. These finite witnesses are not
exhaustive coverage of arbitrary Python control flow.

Named callbacks are accepted by `map`, `flat_map`, `where`, projection expressions
and the iteration step/stop boundaries. A direct value callback invocation requires
a singleton row and cannot introduce effects. Lambda bounds and consumer authority
checks remain in force. Loops without a bounded DAG constructor, recursion, async/decorated callbacks,
function values escaping the DAG, and mixed row/acknowledgement branch returns
remain explicit DAG limitations. A variadic tuple is not a direct entity receiver. Record branches currently require matching field
names; field value contracts are joined recursively. Downstream refinement after
branching remains conservative rather than inventing stronger facts.

Logical evaluation and backend request counts are distinct. Graph hydration and
post-write re-observation may fetch an entity again; the live effect test does not
claim one backend GET per local binding.

## Upstream control-flow evidence

`monty-analysis::function_flow` projects the upstream semantic index into reachable
statement blocks, return exits and conditional successors. The host no longer
collects return paths or reconstructs `if` continuations. Python fallthrough is
`None`. Definite branch selection removes unreachable bodies while preserving
condition evaluation in the reviewed plan. Projection lambdas and named callbacks
use the same invocation adapter, including default captures.

The generic evidence API does not grant Plasm effects or entity authority and does
not execute callback code. Statement nodes that need a new DAG constructor still
fail at the DAG boundary; they are not diagnosed as invalid Python syntax.
