# Python parity in the original language matrix

`python_coverage.json` is a checked ledger over the original matrix, not a second
feature universe. All 162 original rows require executable Python evidence.
Pending and partial entries fail the gate. Supplemental cases cannot substitute
for an original obligation.

```sh
cargo test -p plasm-e2e --test plasm_language_matrix --test plasm_language_matrix_views
```

Use the pinned Monty worker (`PLASM_MONTY_BINARY`) and the repository's debug-test
stack setting (`RUST_MIN_STACK=16777216`). For inventory only, filter the first
suite to `python_coverage -- --nocapture`; that is not live execution evidence.

## Evidence contract

The differential runner obtains native source from `matrix_program_for_row`,
executes both frontends against fresh deterministic fixture HTTP servers, applies
the original planning/live assertions and compares typed values, row order,
coverage and operation evidence. Fixture responses retain real identities and
nested values; comparisons never erase IDs to hide a mismatch.

204 paired/supplemental programs cover the original rows plus Python-specific
cases. Every original feature tag must have all its rows covered. The two non-row
obligations are exercised by `python_host_contract`: production computation
witnesses and actual HTTP wait/cancel behavior. Cancellation interrupts blocked
reads but drains an already-dispatched write without replaying it.

All seven `MATRIX_VIEW_PREFLIGHT_CASES` run Python admission and dry planning,
compare the lowered entity and scope to the original query, and use that actual
query for original view preflight. The views suite also pairs source-bearing
live scope, binding, computed-output and relation tests through both frontends.

The ledger rejects missing/stale rows, changed feature tags, duplicate or dangling
case links, incomplete statuses, absent evidence and changed view/non-row
inventories. Tagged-union tests remain a separately checked supplemental suite.

## Explicit syntax replacements

- Python multiline strings replace heredoc delimiters; paired cases retain text,
  lazy rendering dependencies, reuse, cardinality and failure obligations.
- Typed `@compute` replaces Jinja rendering. Undefined fields and plural scalar
  extraction reject at admission; explicit Python indexing retains runtime errors.
- Qualified Python fields resolve the native binding-name collision. The paired
  test checks the original rejection and the explicitly qualified result.
- `Program.build` returns explicitly. Native last-binding return coercion remains
  an oracle assertion; Python has the same result roots without implicit coercion.
- The native `singleton()` query hint does not truncate rows. Its `.take(5)` pair
  retains those rows; Python grants scalar/receiver authority only to proven
  singletons. Dedicated take-one and empty-singleton rows prove that contract.
- Host `wait(oN)` and `cancel(oN)` are complete transport commands, not DAG source.
  Trailing native/Python statements, extra arguments and malformed handles reject.

## Extending semantics

Extend the original fixture/row first when necessary, add its linked Python case,
and run both whole suites before claiming coverage. Refactor frontend-specific
assertions into shared semantic assertions only when equivalent behavior is
explicitly proved. Do not omit original errors or live-result checks.

This gate proves the original matrix obligations applicable to Python. Compiler
unit tests, adversarial async/effect tests, teaching delivery, packaged-worker
smokes and deployment checks remain additional gates. It is not a live vendor or
LLM benchmark.
