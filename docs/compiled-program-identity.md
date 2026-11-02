# Compiled program identity

`plasm_core::CompiledProgramId` identifies a complete typed `PlasmComp`. The core
owns the algorithm; workflow receipts and promotion verification use the same
infallible constructor. It is separate from plan authorization, catalogue hashes,
source hashes, and the integrity digests of stored evidence.

The `plasm.typed-program.v1` hash domain uses SHA-256 over a Rust structural walk.
The walk never serializes the program. Exhaustive record destructuring and enum
matches make new model fields and variants require an explicit identity decision.
Tags and length boundaries distinguish variants and fields; integers have fixed
width and byte order. An explicit work stack traverses recursive programs.

All program fields participate, including predicates, effects, cardinalities,
ordering, output projections, catalogue ownership, value contracts, provenance,
and metadata. Vectors retain order. Sets and maps use key order, so insertion
order cannot change identity. JSON metadata is traversed as a leaf value algebra;
it is not used as a serialization intermediate for the program.

Invoke inputs have typed and raw internal forms. Persistence currently restores
the raw form. Identity uses their existing Rust lowering to the same operand
value, including union wire mappings. Positive signed and unsigned integers share
a numeric identity; floats remain distinct. Money retains decimal scale, currency,
and transport format. Compiled string environments are derived from source, so
their source is hashed rather than the in-memory environment.

This is conservative program identity, not optimization equivalence. Renaming a
node or changing display metadata can change the ID. Matching intentions and
judging teaching examples remain separate concerns.

This is a full cutover from serialized-byte program IDs. Old seals remain
immutable; their old identities are not accepted through an alternate algorithm.
Historical failed collections remain diagnostic evidence and must not be silently
relabelled as successful promotion runs.

# Pages and learning

The shared execution observer classifies a compiled single-page invocation as
`ExecutionEvidenceKind::ResultDelivery` from its typed IR. It awaits delivery
without opening an execution journal or advancing the learning watermark.
Native and server execution use that observer. Success, failure, cancellation,
and partial page consumption therefore cannot turn a page into a learned program
or leave a page-owned checkpoint blocking review. The originating program retains
its own execution and review evidence.
