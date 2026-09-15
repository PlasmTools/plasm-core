# Hermit source ownership

This is the upstream beavuck-hermit 1.4.26 crate from https://gitlab.com/beavuck-services/hermit, at commit `2c38e795c251db070302b6c144a3547b36afd940`, with the local 1.4.27 spec-response extension. It is vendored so both workspaces use the same modified crate without an unpublished registry dependency. Upstream benchmark-only dependencies and targets are omitted.

`router::build_spec_responses` serves OpenAPI-generated bodies without CRUD request merging or identity injection. Plasm catalog validation uses this mode. Existing CRUD simulation remains available for stateful runtime tests; it is not used as response-contract evidence.

Run `cargo test -p beavuck-hermit` from the OSS root. No upstream release has been published.

CRUD simulation recognizes the standard offset/limit convention only when the OpenAPI operation declares integer query parameters `offset` and `limit`, and its response has the `count`, `next`, `previous`, `results` envelope. Bounds specify total collection size; the request limit specifies page size. Pagination slices a stable collection, sets total count, and terminates continuation. Spec-response mode remains unchanged and does not infer CRUD responses.

Plasm integration shared servers run on a dedicated process-lived Tokio runtime so URL caches remain valid after individual test runtimes shut down. The Berry fixture uses exactly 40 rows to exercise its documented 20-row pages.
