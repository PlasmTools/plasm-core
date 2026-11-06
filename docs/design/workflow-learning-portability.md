# Workflow learning: portable fragments and intent indexing

Status: offline contract verified; conservative Rust implementation in
`crates/plasm-agent-core/src/workflow_learning/portable*.rs` and `index.rs`.

## Existing compiler seams

`crates/plasm-agent-core/src/workflow_program_template.rs` currently supports
parameter holes and entity holes only. It cannot relocate arbitrary Python
methods, relations, fields, annotations, or value-domain references.
`crates/plasm-core/src/symbol_tuning/{keys,tables,session_bindings}.rs` supplies
qualified entity, method and relation identity plus occurrence metadata for
parameters/fields and value domains. Python lowering in
`crates/plasm-agent-core/src/plasm_dag/python/` resolves references in
`catalog_operations.rs`, `expression_captures.rs`, `projection.rs`, `helpers.rs`
and other row operations. Resolution must produce an occurrence ledger before
lowering loses original source locations. A global textual token dictionary is
insufficient: a field or parameter needs its owning entity/capability, and an
ordinary Python identifier or string can resemble a session symbol.

Current-tree qualification: opaque `p#` tokens are no longer assigned
(`symbol_tuning/mod.rs`, `capability_param_quad_for_p_sym`); fields and parameters
use wire names. `v#` value-domain symbols are teaching metadata, not presumed
Python identifiers. The production relocation walker must cover actual admitted
entity/method/relation references (`e#`/`m#`/`r#`) through `SymbolSession` typed
reverse resolvers and preserve wire field/parameter names. Semantic field,
parameter and value dependencies still belong in the dependency contract, but
the generic offline all-kind witness is not a claim that all six token kinds
are currently executable language syntax.

## Portable contract

* `SemanticRef(kind, catalog, owner, member)` identifies an entity, capability,
  relation, field, parameter, or value domain. Production uses existing typed
  identity wrappers; `owner` includes capability identity for input parameters.
* `Occurrence(start_byte, end_byte, expected_token, semantic_ref)` comes from
  successful compiler resolution, including annotations and generated captures.
  Capture source digest, catalogue contracts, compiler version and completeness
  attestation with the ledger. Missing resolution prevents portability.
* `LiteralSlot(start_byte, end_byte, role, type, equality_group)` is an approved
  parameter occurrence. Only leaf values selected by semantic analysis become
  neutral; constants governing guards, effects, bounds and currency remain
  significant unless equivalence is separately established. A repeated slot
  retains one equality group. Typed domain constraints accompany slots.
* `rebind(fragment, current_exposure) -> PythonSource | UnresolvedReference`
  replaces validated, nonoverlapping source ranges in reverse byte order.
  Resolve *every* reference against the current authorized exposure, then run
  normal Python admission/typechecking with the dependency closure. Never alter
  quoted text or comments merely because it resembles a symbol. Serving does
  not execute the program or approve its effects.
* `canonical_shape(fragment) -> StructuralFingerprint` replaces semantic
  reference occurrences with stable identities and approved literals with typed
  role/equality-group markers, then canonicalizes the parsed Python AST. Keep
  operation/control-flow order, operators, unapproved constants, and binding
  relationships. This is exact structural deduplication, not semantic proof.
  The offline prototype deliberately leaves local-variable alpha-renaming out:
  a production implementation needs lexical scope analysis, not name sorting.

The dependency closure contains referenced declarations and all types, relations,
capability inputs/outputs and value constraints needed to admit the fragment;
minimality is measured after the actual compiler accepts the closure. The
compiler is authoritative; a Python parser alone is not a Plasm typechecker.

## Two-stage retrieval

An LLM proposes normalized intent facets (action, object, direction, conditions,
preservation requirements and subtask roles), with evidence spans. Literal
values are excluded from the family key only when mapped to approved roles.
Catalogue identities and capability contract fingerprints form a separate
variant key. The same payment family can have Venmo and another provider's
variants without treating either as executable through the other.

Retrieval returns candidates, never permission. First enforce tenant/access and
explicit requested-provider constraints; score remaining semantic facets and
subtask coverage. A family match may guide discovery toward allowed catalogues.
Serving still requires current authorization, complete rebinding and typecheck.
An empty authorized set yields no candidates. Catalogue signature drift makes
the stored variant require revalidation, not a silent semantic substitution.

`scripts/workflow_learning/test_portability_index.py` is a dependency-free
executable model using supplied, trusted occurrence ledgers. It checks byte
range safety, all reference kinds, repeated slot identity, literal neutrality,
guard distinction, provider variants, and authorized retrieval. It does not
implement LLM normalization, extract compiler references, validate complete
dependency closure, establish general semantic equivalence, or measure learned
retrieval quality. Those remain separate integration/evaluation gates.

Run: `python3 scripts/workflow_learning/test_portability_index.py`.

## Initial Rust admission boundary

The Rust constructor compiles the source, verifies source/bundle correspondence,
and records typed entity/method/relation AST occurrences. It rejects opaque
symbol shadowing, recorded-output access and stringized annotations. A serving
rebind verifies entire dependency catalogue hashes, resolves each reference
through the live symbol map, compiles the result, and reconstructs its reference
ledger to verify stored completeness. Field and parameter wire names are retained.

`PortableProgram::parameterize` is the learned-example constructor. Its initial
proof covers scalar string/integer/float literals in direct entity capability
keyword arguments (including resolved query/Get acquisition), outside
branch/loop/conditional scopes. It preserves the source structure and represents
inputs as typed AST literal slots with qualified roles and equality groups.
Replacing slots with original inputs must restore the exact canonical source;
canonicalization itself must preserve the compiled computation. Synthetic
demonstration inputs are compiled separately. No extra build-default bindings or
DAG nodes are introduced. Other data literals conservatively prevent promotion;
integer `take` bounds remain structural. Sensitive and governance-related keyword
names are excluded. Original evidence is retained separately under its access
scope. Typed rowset inputs, relation materialization closure and more complex
parameterization remain rejected until their proofs are implemented.

`rebind_portable` returns both checked source and input byte ranges relocated for
the current symbols. `instantiate` substitutes typed inputs and symbols in one
edit pass and runs ordinary admission; it does not execute. The source version,
compiler package version and comp-wire version must match, otherwise serving
returns a revalidation requirement. Required capabilities are derived from the
compiled computation, including implicit query/Get acquisition; output contracts
retain the compiler's result shape and recursive row type.

Existing workflow template execution uses the same typed reference resolver.
Additional holes are `{{sym:entry.Entity::method:capability}}` and
`{{sym:entry.Entity::relation:relation}}`. Raw session symbols in template code
are rejected. HTTP executable instantiation now requires a tenant-owned live
`logical_session_ref`; synthetic symbol assignment has been removed. MCP uses
the same live symbol map before its normal dry-run admission.

Catalogue distribution hashes are retained as validation metadata, not blanket
serving rejection gates. Rebinding compiles against the current catalogue and
compares referenced capability (including effect/transport mapping), entity,
and shared security/value contracts. Distribution version and unrelated entity
or capability additions do not invalidate an otherwise unchanged program.
Referenced semantic changes that still typecheck remain repair candidates.

Replacement additionally requires equal behavioral fingerprints: canonical AST
with semantic symbols and proven literal slots, preserving predicates, effect
order, bounds, and nonparameter constants. Input/output/capability coverage alone
never proves two programs interchangeable. This conservative first version uses
the same fingerprint for neutral identity, so equal behavior normally adds
supporting evidence; structural optimizer replacement awaits a separately
verified preservation witness. Unmeasured robustness, unnecessary-call and
readability dimensions remain neutral rather than inferred from shorter code.
