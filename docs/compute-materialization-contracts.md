# Compute materialization contracts

An unannotated helper consumes the exact typed values at its call site. Its
arguments use the existing dependency packet, including a single argument.
Python input stubs, admission and runtime materialization use the same recursive
contract. The compiler does not manufacture a `Row` annotation and then try to
recover a nested value from that placeholder. Authored annotations remain checked
refinements; they do not grant entity authority.

Authored return annotations are checked before materialization, and unannotated
returns acquire their contract from whole-body inference. Final Python admission
checks executable definitions without imposing a synthetic serialization return
annotation. Mutable Python container invariance must not reinterpret codec choices
as authored type constraints. Runtime results still validate against the established
materialized output contract.

Collection axes are preserved independently. Packing arguments does not join,
flatten or zip their rows. The compute row budget counts materialized values
through nested containers, so packaging cannot hide rows from the existing limit.

Mutation through a container lookup retains its owning initializer dependency.
The checker projects receiver evidence back through those container axes and
uses it to close nested literal element types. Resolved scalar collections still
reject incompatible updates, and recursive or unresolved values do not become
`Any`. This uses source patches to the existing `ty_python_core` and
`ty_python_semantic` 0.0.14 dependencies; their versions remain pinned. Source
provenance and licenses live beside each vendored crate.

Inferred type graphs are decoded with an explicit worklist and cached results
shared across roots. Cycles and invalid references are typed failures. Alias-chain
length does not consume the Rust call stack. Get call-shape validation precedes
input resolution, so positional capability inputs are diagnosed at the call site.
Language diagnostics describe Plasm operations, entity identities, declared
capability inputs and observations. Provider transport details belong to
transport diagnostics. The `get` operation selects an entity observation using
the capability's declared signature; its implementation may use any supported
provider transport.

## Validation

The regressions use abstract fixtures, without model calls or production catalogs:

- Native checker cases for dictionary-held lists, nested list mutation, nested
  dictionary assignment and mixed scalar/collection results.
- Property coverage for nested field provenance, cardinality and result values.
- Public Python admission and native execution with zero, singleton and plural
  acquired rows, independent arguments and computed results.
- An 8,192-link inferred alias chain, cycles and malformed references.
- Rejection of excess nested input rows and positional inputs to a `get` capability
  that declares no entity identity argument.

From the OSS workspace, build the matching worker before execution tests:

```sh
export CARGO_TARGET_DIR="$PWD/target"
cargo build --manifest-path vendor/monty/Cargo.toml -p monty-runtime --bin monty --no-default-features --locked
export PLASM_MONTY_BINARY="$CARGO_TARGET_DIR/debug/monty"
cargo test -p monty-analysis --locked
cargo test -p plasm-agent-core python_compute --lib --locked
cargo test -p plasm-agent-core map_body::tests::python_lowering --lib --locked
cargo test -p plasm-agent-core get_without_identity_rejects_positional_capability_inputs --lib --locked
```

These checks establish compiler and runtime behavior. They do not measure changes
in model task completion; frozen evaluation outcomes remain separate evidence.
