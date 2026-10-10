# Agent error contract

An agent error is a correction, not a Rust diagnostic dump. State the failed
contract, its relevant entity/field/operation, and the valid next action. Preserve
all independent constraints; do not truncate a list of required corrections.
Group repeated evidence reasons rather than printing every occurrence.

Keep typed variants, causal sources, evidence identities, and effect receipts in
their owning layers. Never render ASTs, collection hashes, response objects,
untrusted input strings, or internal structs with `Debug` in an error message.
Schema names, required keyword lists, semantic types, and effect reconciliation
receipts are useful facts, not permission to echo arbitrary values.

| Failure | Owner | Presentation |
| --- | --- | --- |
| Missing catalog membership proof | CGS compilation | Name the entity/relation; require a verified exhaustive embed or paginated scoped acquisition. Reject the catalog before exposure. |
| Invalid Python program | Admission | Identify the violated signature/type and the correction. |
| Incomplete observed response or pagination | Runtime | Explain the missing observation/termination proof; do not classify it as invalid Python. |
| Provider rejection | Transport/runtime | Extract all recognized messages and field constraints; never serialize arbitrary provider JSON or HTML into the correction. |
| Completed or uncertain writes | Runtime/host | Preserve typed receipts and reconciliation guidance. A corrected program must not silently replay completed writes. |

`scripts/ci/check-agent-error-presentation.py` rejects Debug format specifiers in
the shared error definitions. Presentation regressions also cover opaque provider
payloads, many evidence gaps, malformed handles, and MCP text/structured failure
delivery. Logs may retain diagnostic detail; logs do not replace a correction.
