# Python scoped composition contract

Status: recursive bounded maps are implemented, including ancestor captures,
synthetic record/array results, compute-to-write dependencies, synthetic inputs, effect
receipts and full occurrence paths. Raw catalog-row arrays have open shape/materialization obligations in
[the conformance design](python-conformance.md). The ledger remains conservative: executing
nested programs does not discharge every adversarial law obligation below.

This contract governs outer Python DAG construction. The
[compute contract](python-compute-contract.md) governs code inside `@compute`.
Neither boundary executes arbitrary outer Python.

## Semantic model

A body is an ordinary typed DAG with explicit input ports, a result root and
effect dependencies. Nesting introduces a lexical scope and an occurrence; it
does not introduce a second evaluator or a special collection of allowed body
operations. An operation available at the root is available in a body when its
ordinary type, cardinality, authority and capability requirements hold.

The compiler resolves lexical names to scope-qualified ports before producing
IL. Runtime execution receives only the sealed ports and body. It must not
search an enclosing mutable environment by Python spelling.

Canonical `PlasmComp` wire version 3 carries a shared `CaptureContract` on every
parent and enclosing port. `Value` retains the recursive value type, including
null and arrays, and cannot carry entity receiver authority. `Rows` carries the
observed schema and receiver authority. Both ports implement `CapturePort` for
construction-independent validation and lowering. Cardinality remains separate:
one parent occurrence is not proof that its enclosing source is a singleton.
Selection, ordering, deduplication and limits preserve value kind. Earlier comp
wire versions are rejected; their capture representation is not reinterpreted.

The existing `map` cardinality is retained: one synthetic record per source row.
A nested map used as a record field contributes a typed array of records,
including an empty array. It does not flatten or drop its parent. Bare rowset
returns from a map lambda remain invalid; use `flat_map` for a rowset result.
`flat_map` uses the same sealed body and capture mechanism, concatenates child
rows in parent order, preserves receiver identities, and combines source and
child coverage without upgrading incomplete observations. Its default parent
ceiling is 65,536; an explicit `max_parents` may lower that bound. Both operations
share the run-wide 65,536-occurrence ceiling and 16-level nesting limit.

Captured f-strings lower to typed compute nodes. Each captured field becomes an
explicit singleton input; Python methods, conversions and format specifications
are checked and executed by the compute contract. They never run in the host
while the DAG is being built. Lambda parameter spelling does not change the
semantic commit identity.

Flat-map surface failures retain prior effects and per-parent failed outcomes,
skip the failed occurrence's remaining dependents, continue independent parents,
and produce partial coverage. Cancellation stops new invocations. Record maps
retain fail-fast behavior; neither form rolls back completed effects.

Supported composition shape (symbols and fields are session-specific):

```python
rows = parents.map(
    lambda parent: {
        "parent_id": parent.id,
        "children": parent.r1.map(
            lambda child: {
                "parent_title": parent.title,
                "child_title": child.title,
            },
            max_parents=32,
        ),
    },
    max_parents=256,
)
```

The types and relation token in this illustration require corresponding served
declarations. The example specifies composition, not a catalog or a passing test.

## Laws and required witnesses

### SC-01 — Lexical capture

```plasm-law
{
  "id": "SC-01",
  "extends": ["lang_cross_binding_render", "lang_ra4_apply_foreach_bind_cut"],
  "checks": [
    {"role": "positive_live", "claim": "Current, ancestor and root captures retain lexical ownership", "evidence": [{"kind": "matrix", "id": "scoped_nested_records"}, {"kind": "matrix", "id": "scoped_nested_effects"}, {"kind": "matrix", "id": "scoped_flat_map_format"}], "gap": "These registered cases establish representative behavior only; the full cross-product in the law remains unverified."},
    {"role": "negative_admission", "claim": "Shadowing, undeclared ports and plural scalar extraction reject", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Sibling scopes and empty captures keep distinct addresses and cardinality", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Alpha-renaming and lawful factoring preserve the semantic plan", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

A body may use its current row and enclosing immutable bindings. Every free
reference becomes an explicit typed capture dependency. A current row is a
singleton for that occurrence; a captured rowset retains its original
cardinality. Capturing a plural rowset must not make `rows.field` scalar.

Lambda parameters may not shadow an enclosing binding, reserved name or served
entity symbol, consistent with existing fanout admission. Independent sibling
scopes may reuse local names. Runtime port identities distinguish them.

Witnesses: current row, parent, grandparent, root singleton and plural captures;
sibling reuse; shadowing rejection; undeclared capture and alias tampering.

### SC-02 — Recursive typing and authority

```plasm-law
{
  "id": "SC-02",
  "extends": ["lang_bind_projection_then_relation", "lang_federated_duplicate_entity_relation_r"],
  "checks": [
    {"role": "positive_live", "claim": "Projected recursive domains and receiver authority survive captures", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "negative_admission", "claim": "Projected-away fields and synthetic receivers reject at every depth", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Absent, null, unavailable, compound and federated values retain distinctions", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Moving an operation into a scope preserves the same typed contract", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

Capture preserves the source's actual projected schema, primitive and named
domains, nullability, tagged unions, nested records and array element types.
An absent projected field cannot reappear through the full catalog schema.

Entity authority is carried separately from values. A catalog row with lawful
identity can remain a receiver through a capture. Synthetic records, including
records with an `id` field, cannot acquire authority. Compound identity and
catalog qualification remain intact at every depth.

Witnesses: projected-away fields; nullable and nested values; captured entity
receiver versus synthetic lookalike; compound identity; identical entity names
in distinct catalogs; a payload row distinct from the mutation receiver.

### SC-03 — Map shape

```plasm-law
{
  "id": "SC-03",
  "extends": ["lang_ra4_apply_derive_message_field", "lang_relation_empty_fanout"],
  "checks": [
    {"role": "positive_live", "claim": "Nested maps preserve parent-child pairing and one record per parent", "evidence": [{"kind": "matrix", "id": "scoped_nested_records"}, {"kind": "matrix", "id": "scoped_nested_effects"}, {"kind": "matrix", "id": "scoped_flat_map_format"}], "gap": "These registered cases establish representative behavior only; the full cross-product in the law remains unverified."},
    {"role": "negative_admission", "claim": "Invalid plural/entity-authoritative map outputs reject", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Zero, one and many parents and children preserve order and empty arrays", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Equivalent nested and factored construction preserves output shape", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

Each occurrence returns exactly one synthetic record. Source order determines
output order. Empty source produces zero records and invokes no body. An empty
child source contributes `[]` to its parent's record without dropping that
parent. Nested output types are known before execution, including empty results.

Witnesses: zero, one and many parents and children; distinct values on both sides
of every parent/child pairing; nested records and arrays; invalid plural or
entity-authoritative output; two and three levels of maps.

### SC-04 — Recursive operation closure

```plasm-law
{
  "id": "SC-04",
  "extends": ["lang_ra4_apply_monolith", "lang_ra4_apply_bind_cut", "lang_ra4_apply_relation_monolith"],
  "checks": [
    {"role": "positive_live", "claim": "Ordinary reads, relations, compute and effects compose recursively", "evidence": [{"kind": "matrix", "id": "scoped_nested_records"}, {"kind": "matrix", "id": "scoped_nested_effects"}, {"kind": "matrix", "id": "scoped_flat_map_format"}], "gap": "These registered cases establish representative behavior only; the full cross-product in the law remains unverified."},
    {"role": "negative_admission", "claim": "The same invalid operand rejects at root, child and grandchild", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Each operation retains values, coverage and effect receipts at each depth", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Root and scoped interpretations agree under lawful captures", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

Read, relation, compute, derivation and mutation nodes retain their existing
semantics inside a scope. The implementation uses the ordinary node lowering,
validation and execution machinery recursively. It must not introduce a separate
body expression whitelist or one special lowering for an attachment workflow.

The closure claim applies to typed DAG operations, not arbitrary Python statements.
Dictionary fields construct values and their dependencies; they do not provide
Python statement sequencing. General source-level block/helper syntax requires
its own admission contract before it can be taught.

Witnesses: the same lawful operation at root, child and grandchild depths;
equivalent factored computations; invalid operands rejected at each depth.

### SC-05 — Explicit local bounds

```plasm-law
{
  "id": "SC-05",
  "extends": ["lang_iterate_bound_exhausted", "lang_per_row_render_zero"],
  "checks": [
    {"role": "positive_live", "claim": "Per-invocation bounds are explicit and checked", "evidence": [{"kind": "matrix", "id": "scoped_nested_records"}, {"kind": "matrix", "id": "scoped_nested_effects"}], "gap": "These registered cases establish representative behavior only; the full cross-product in the law remains unverified."},
    {"role": "negative_admission", "claim": "Missing and invalid literal bounds reject", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Boundary and bound-plus-one checks preserve completed prefixes", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Local bounds are invariant under sibling scope renaming", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

Every map requires a literal `max_parents` in 1–256. The bound applies to the
number of source rows for each invocation of that map, not to the cumulative
number of rows across all invocations. Exceeding it is an error, never truncation.

The source must be materialized and checked before starting that invocation's
body. A nested bound failure may occur after earlier parent effects have
completed; it does not imply global rollback or that no write occurred. Static
admission is separate from data-dependent runtime bound checks.

Witnesses: exactly at and above each bound; outer and inner failures; empty
sources; failure in a later parent with the earlier durable prefix preserved.

### SC-06 — Shared execution limits

```plasm-law
{
  "id": "SC-06",
  "extends": ["lang_iterate_until_bound"],
  "checks": [
    {"role": "positive_live", "claim": "Occurrences share run-wide limits", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "negative_admission", "claim": "Excessive structural depth rejects including replay", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Nested cancellation and aggregate budget exhaustion stop new work", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Factoring does not mint a new execution resource budget", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

Multiplying local bounds does not grant unlimited aggregate work. All occurrences
share the enclosing execution's cancellation, timeout, transport and compute
resource limits. Admission rejects more than 16 nested map levels, including
replayed artifacts. A run-wide atomic budget admits at most 65,536 map
occurrences across all roots, siblings and descendants. An exhausted budget
fails before starting the next occurrence; completed writes are not rolled back.
These limits are distinct from each map's local `max_parents`.

Witnesses: cancellation during inner reads/compute/writes; deeply nested input
rejected within the structural limit; worker and transport limits shared across
siblings. A parent count bound is not evidence of a total resource bound.

### SC-07 — Collection completeness

```plasm-law
{
  "id": "SC-07",
  "extends": ["lang_relation_many_from_plural_query", "lang_union_rowset"],
  "checks": [
    {"role": "positive_live", "claim": "Complete and empty-complete sources materialize without promotion", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "negative_admission", "claim": "Static false completeness claims reject on replay", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Unknown, partial and continuation-bearing sources remain distinct at each depth", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Relation, projection, union and map compositions do not strengthen coverage", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

Capture, derivation and nesting do not promote Unknown or Partial to Complete.
Operations that require complete inputs retain that requirement at every depth.
An empty complete child and an unavailable/unknown child are distinct outcomes.
Continuation-bearing inputs cannot be admitted as fully materialized collections.

Witnesses: complete, empty-complete, unknown, partial and continuation-bearing
sources at each level; relation/set/filter/map/compute chains. Vendor metadata is
independent evidence and cannot be inferred from these fixture tests.

### SC-08 — Effects and ordering

```plasm-law
{
  "id": "SC-08",
  "extends": ["lang_program_return_consecutive_writes", "lang_ra4_apply_foreach_monolith"],
  "checks": [
    {"role": "positive_live", "claim": "Nested writes remain in the reviewed effect summary", "evidence": [{"kind": "matrix", "id": "scoped_nested_effects"}], "gap": "These registered cases establish representative behavior only; the full cross-product in the law remains unverified."},
    {"role": "negative_admission", "claim": "Forged effect labels and removed ordering edges reject", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Wire order, unreturned writes and post-write freshness are preserved", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Object key order and value factoring cannot reorder effects", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

A scope's effect summary is derived recursively from its body. A map containing
a write is not read-only, even when its result is synthetic. Review, approval,
scheduling, retention and replay all consume that same summary.

Data dependencies and explicit effect-order edges determine the body schedule.
Unreturned effects remain in the plan. Sibling effectful occurrences execute in
source order, each finishing before the next begins; an enclosing dependent
read waits for all preceding nested effects. Lexical evaluation order of effect-producing expressions is compiled into
explicit effect-order edges; serialized object-key order is not the schedule. A create-directory then download sequence needs a sealed
ordering dependency even if the download does not consume the create result.

Witnesses: unreturned nested writes; parent and child receivers; ordered wire
ledger; read-after-write freshness; recursive dry-review effect summary;
tampering with effect labels or removing ordering edges rejected before I/O.

### SC-09 — Failure, cancellation and receipts

```plasm-law
{
  "id": "SC-09",
  "extends": ["lang_iterate_bound_exhausted", "lang_render_value_error_at_execution"],
  "checks": [
    {"role": "positive_live", "claim": "Successful nested effects retain receipts", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "negative_admission", "claim": "Statically invalid later nodes prevent all backend IO", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Failures and cancellation preserve durable prefixes and uncertain outcomes", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Equivalent plans expose equivalent effect traces including failure prefixes", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

Failure stops dependent work and subsequent effectful occurrences. Completed
external writes remain completed. Their receipts must remain observable when a
later body fails or is cancelled. Error reporting identifies the full occurrence
address and distinguishes completed, failed, cancelled and not-invoked work.
Cancellation must not cause an in-flight uncertain write to be blindly replayed.

Witnesses: failure before the first write; a rejected second/third write;
cancellation during a write; durable prefix and receipts; no later writes or
dependent reads; retry/replay behavior.

### SC-10 — Occurrence identity and streaming

```plasm-law
{
  "id": "SC-10",
  "extends": ["lang_relation_empty_fanout", "lang_apply_get_multirow"],
  "checks": [
    {"role": "positive_live", "claim": "Full occurrence paths identify nested work", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "negative_admission", "claim": "Forged occurrence/scope identities reject where admitted", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Live events precede completion and survive cancellation without collisions", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Sibling local-name reuse does not merge occurrence addresses", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

An occurrence address is the full path of scope IDs and row ordinals from root
to leaf. Entering a child appends to both paths. Repeated child IDs or local step
names under different parents must never collide. Streaming and stored evidence
use the same address; progress remains available before completion or failure.

Witnesses: two parents with identical child IDs and local names; sibling maps;
three nesting levels; live progress before an inner operation completes;
cancellation preserving already emitted evidence.

### SC-11 — Whole-program admission and review identity

```plasm-law
{
  "id": "SC-11",
  "extends": ["lang_render_undefined_field", "lang_render_content_plural_reject"],
  "checks": [
    {"role": "positive_live", "claim": "Recursive admission checks all bodies before review", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "negative_admission", "claim": "Invalid empty/later bodies and tampered captures/schema/bounds reject", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Replay revalidates profile, catalog and effect seals before IO", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Display-only and alpha-renaming changes preserve semantic identity", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

Admission recursively checks all scopes and all compute callsites before any
backend I/O, including a body whose runtime source will be empty. It validates
captures against enclosing contracts rather than trusting serialized claims.

Review identity includes typed ports, recursive schemas, catalog ownership,
bounds, body operations and effect edges. Stored artifacts are revalidated before
execution. Changing display metadata alone does not change semantic identity.

Witnesses: invalid later/empty body gives zero calls; tampered capture, schema,
bound, catalog and effect edge reject; round-trip and replay preserve the plan.

### SC-12 — Value/effect separation

```plasm-law
{
  "id": "SC-12",
  "extends": ["lang_federated_auth_session_provides_mutation", "lang_program_return_consecutive_writes"],
  "checks": [
    {"role": "positive_live", "claim": "Acknowledgements and entity-returning effects retain different types", "evidence": [{"kind": "matrix", "id": "scoped_nested_effects"}], "gap": "These registered cases establish representative behavior only; the full cross-product in the law remains unverified."},
    {"role": "negative_admission", "claim": "Acknowledgements and synthetic records cannot become receivers", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "runtime_evidence", "claim": "Empty and failed fanout preserve acknowledgement counts and receipts", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."},
    {"role": "metamorphic", "claim": "Embedding an effect result preserves its declared value/effect separation", "evidence": [], "gap": "Required law-specific witnesses are not yet registered in this literate ledger."}
  ]
}
```

A side-effect-only capability produces its declared acknowledgement, not an
invented entity with blank fields. A value-returning mutation retains its declared
result type. A containing synthetic record may carry typed results but cannot
turn an acknowledgement into an entity receiver. A side-effect call in a map record field yields the typed record
`{"completed": int, "failed": int}`, derived from operation acknowledgements.
The complete operation ledger remains attached to the enclosing map result;
an incidental API response body is not promoted into an entity value.

Witnesses: side-effect-only acknowledgement; entity-returning create; downstream
receiver admission; returned receipt counts and values; empty and failed fanout.

## Implementation audit

| Boundary | Implemented behavior |
|---|---|
| `plasm_dag/python/body.rs` | Ordinary DAG lowering in scoped overlays; explicit capture ports; recursive record values |
| `plasm_monad/correlated.rs` and `correlated/scope.rs` | Recursive scope/dependency validation, effect classification and structural limits |
| `plasm_step_convert::lift_body` | Typed ports retain schemas, singleton contracts and separate receiver authority |
| `map_body_schema.rs` | Recursive record/array and acknowledgement schemas derived from body results |
| Plan effect classification and flow | Nested effects participate in root review/order; ancestor flow facts seed captured ports |
| `plasm_plan_run/map_body.rs` | Isolated environments, full addresses, shared occurrence budget, collected nested operation receipts |
| Matrix | `scoped_nested_records` and `scoped_nested_effects` run in the original live suite; agent-core adds adversarial witnesses |

Focused host witnesses include three-level ancestor capture, compute from combined
parent/child records, compute-to-write values, empty collections, synthetic inputs
without receiver authority, inner-bound failure after a durable prefix, pre-I/O
invalid bodies, capture/schema/bound tampering, and streaming cancellation inside
an inner read. Original single-level map witnesses remain required.

The full law inventory is not declared complete. Cross-catalog value captures,
all operation/depth combinations, compound-key captures and cancellation during
nested writes need their own recursive witnesses before the corresponding law
can be marked fully covered. Root literal-binding capture and general statement
blocks inside map lambdas are not supplied by this implementation.

## Matrix linkage and completion gate

The `plasm-law` blocks beside each law are the canonical executable ledger.
The matrix parses this document directly; there is no parallel JSON inventory.
Each law extends named original matrix rows. Those rows remain independently
required; new composition witnesses do not replace their assertions.

Status is derived from each check's registered evidence and explicit remaining gap.
A law remains partial while any check has a gap; no evidence means open. All four
roles (positive live, negative admission, runtime evidence and metamorphic) are
required. Discharging these finite obligations is not proof over arbitrary programs. A test that verifies rejection of a required
positive program is a gap reproduction, not conformance evidence. Documentation,
counts, test names or the existence of a JSON ledger alone cannot prove coverage.

Completion requires the original language and view suites, scoped positive and
adversarial runtime suites, review/replay checks, and packaged worker tests to
pass together. AppWorld authored programs are subsequent integration evidence;
they neither define the language nor substitute for the abstract matrix.
