# Value codec contracts

Provider observations, program literals, structural identity slots and persisted values
are distinct boundaries. A program literal may use explicitly supported syntax
coercion. An observation must satisfy its resolved catalog contract; a failed decode
must not manufacture null, an empty collection, a string, or a singleton array.

## Contract and ownership

`ResolvedValueContract` resolves the existing named value definition and catalog pin,
including nested array domains and the target entity's identity fields. Its fields
are private. `ValidatedValue` contains the admitted value and that contract; only
codec admission constructs it. Encoding under a different contract fails.

`ValueCodec` defines fallible `decode` and `encode` operations with an associated
wire type. The adapters are:

| Adapter | Boundary | Rule |
| --- | --- | --- |
| `ValueWireCodec` | Concrete native observations | Validate shape, profiles, currency and recursive constraints. Reject executable operands. |
| `JsonValueCodec` | Provider/outbound JSON | Lift objects structurally. Interpret decimal money under its contract without an intermediate float. |
| `IdentitySlotCodec` | Text stored in structural identity slots | Parse according to the declared identity type; preserve numeric, boolean and string distinctions. |
| `StorageValueCodec` | Internal persistence | Preserve semantic money metadata and unsigned representation. Escape objects resembling internal tags. |

For every successful encode, decoding the wire under the same contract must recover
the validated value. Profiles can normalize admission, so this is a value round-trip
law, not a requirement to reproduce the input's original textual spelling. A wire
format that cannot preserve an admitted value returns `LossyWireRepresentation`.

Provider JSON never interprets storage tags. Storage decoding rejects malformed tags;
it does not treat malformed currency or format metadata as absent. Nested failures
retain field and element locations. Non-finite numbers and unresolved operands are
rejected before transport or fingerprinting.

## Rows, execution and presentation

`RowCodec` restores typed simple and compound identities from the owning catalog.
An explicitly untyped row may retain an opaque textual identity; a supplied catalog
with a missing target is an error. Numeric references remain numeric through provider
decode, relation materialization, cache values and compute membership tests.

Row materialization, cache serialization, artifacts and HTTP result construction
propagate codec errors. Presentation is a terminal projection; it cannot silently
turn an encoding failure into a successful empty result. HTTP requests, mock
requests, replay fingerprints and resolved operand persistence use checked encoders.
Fingerprinting rejects invalid values rather than aliasing them with null.

`ExecutionCollection` carries an explicit `RowMaterialization` contract independently
of membership, residency and delivery. Catalog rows use catalog identity restoration
and relation hydration. Computed rows retain their recorded fields without acquiring
catalog identity or relation semantics. Value-producing operators state the output
contract; flat-map and concatenation reject incompatible contributing collections.
Delivery, capture and rehydration preserve the contract. Run artifact schema version 5
requires it, so restored computed pages cannot accidentally enter catalog hydration.
Older artifact versions remain on disk but are not admitted by the new decoder.

These rules preserve information and type meaning. They do not choose task entities,
reinterpret catalog relationships, infer unseen work, retry effects or change an
agent's semantic selection. Abstract fixtures and codec laws verify mechanics;
model-behaviour effects require a separately authorized measurement.
