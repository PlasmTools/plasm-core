# Glossary

| Term | Meaning |
|------|---------|
| **CGS** | Capability Graph Schema — `domain.yaml` semantic model (entities, relations, capabilities; split catalogs use **`values:`** + **`value_ref`**). |
| **`.with`** | Row-compute postfix that adds derived columns per row (`.with{col: expr}`) while preserving entity identity — see [Row compute](reference/plasm-row-compute.md#derived-columns-with). |
| **RowPlan** | Fused execute-time IR for row compute (`plasm_core::row_plan`); Polars-backed evaluation in `plasm_runtime::row_compute`. |
| **CML** | Capability Mapping Language — `mappings.yaml` wire templates. |
| **teaching table** | Symbol-tuned teaching text (`e#` / `m#` / `r#` plus **wire names** for fields/params; `v#` gloss only) for agents. |
| **view** | CGS **`views:`** entry — composed read-only DAG over existing capabilities (not MCP tenant “registry views”). |
| **schema overlay** | **`schema_overlay:`** block — session-open merge of workspace-specific typed columns or entities. |
| **registry `entry_id`** | Identifier for one packed catalog entry in multi-entry mode. |
| **logical session** | MCP `plasm_context` / execute session key fan-out for traces and reuse. |
| **`prompt_hash` / `session`** | Execute-session identifiers pinning one instruction bundle. |
| **plasm-server** | OSS appliance binary — in-process kernel, HTTP/MCP, optional Ratatui control station. |
| **control station** | Ratatui operator UI in **`plasm-server`** (Status · Clients · APIs · OAuth · Keys · …). |
| **remote terminal** | The **`plasm`** CLI — HTTP discovery/execute client (`init`, `search`, `context`, `run`). |
| **client-owned symbol space** | With **`plasm`**, monotonic `e#`/`m#`/`r#` teaching rows (plus wire-name field/param gloss) live in the client mirror (`.plasm/`), not on the server session alone. |
| **MCP transport key** | Bearer API key for Streamable HTTP when MCP configs exist. |
| **incoming auth** | Optional JWT / API key plane for tenant-scoped execute identity. |
