# How Plasm fits together

If you only remember one sentence: **Plasm models an API as a graph, compiles agent-sized programs against that graph, and runs them through concrete HTTP (or GraphQL) mappings.**

Use this page before diving into CGS/CML jargon elsewhere.

---

## Running example: “GitHub Issue”

Imagine an agent should **show issue #42** in a repo.

1. **Conceptually** there is an entity type like `Issue` with fields (`title`, `state`, …) and relations (`repository`, `assignees`).
2. **Operationally** “show” becomes an HTTP `GET` with path `/repos/{owner}/{repo}/issues/{number}`.
3. **For the agent**, you don’t hand-write URLs each time—you expose the graph as **teaching table** rows (`e1`, `e2`, …) and let the agent write a tiny **Plasm** expression such as `e1.title` or a short pipeline.

Once that clicks, the formal names below are just labels for those layers.

---

## CGS (Capability Graph Schema): what exists and what can be done

The **CGS** describes:

- **Entities** — Nouns in the domain (`Issue`, `Repository`, …).
- **Fields & relations** — Data on those entities and links between them.
- **Value domains** — In split `domain.yaml`, top-level **`values:`** holds **semantic slots**; fields and parameters use **`value_ref`**.
- **Capabilities** — Observable actions or queries (`get_issue`, `list_issues`, …).
- **Views** — Optional **composed read-only** DAGs over existing capabilities (dashboards, scoped snapshots). See [Composed views](authoring/views.md).
- **Schema overlay** — Optional **`schema_overlay:`** merges **user-defined columns** (Notion properties, Jira custom fields, …) at execute session open. See [Schema overlays](reference/schema-overlay.md).

Think of CGS as *the contract the agent reasons about*. It is authored as YAML (`domain.yaml`) and loaded into the runtime.

---

## CML (Capability Mapping Language): how calls hit the wire

**CML** attaches **transport-specific templates** to each capability—typically REST paths, methods, headers, and JSON bodies (`mappings.yaml`).

- Same CGS idea (“get this issue”) can map to **REST today** and **GraphQL tomorrow** with different mapping files.
- View capabilities use **`transport: view`** stubs; the runtime executes the declared DAG instead of HTTP templates.

---

## Plasm language: what agents actually write

Agents write **Plasm** programs against symbols exposed in **teaching table** instructions:

- **`e#`** — Entity types (nouns in the active catalog).
- **`m#`** — Methods / capabilities (queries, gets, writes, actions).
- **Wire names** — Fields, capability parameters, query filters, and projections (catalog left-column names such as `name`, `id`, `title`, `owner`, `repo`).
- **`r#`** — Relation navigation hops (fanout to related entities).
- **`v#`** — Value-domain teaching gloss only (never emit in programs).

Legacy opaque `p#` tokens for fields/params are **rejected** at parse.

Expressions compose with pipes and postfix transforms. Multi-line payloads use tagged **heredocs** — see the [Language definition](reference/plasm-language-definition.md).

With the **`plasm`** remote client, the **client owns the monotonic symbol table** locally; the server executes expanded programs over HTTP. See [Remote terminal](reference/plasm-cgs-remote-terminal.md).

---

## Runtime + host: three OSS surfaces

| Surface | Binary | Role |
|---------|--------|------|
| **Appliance** | **`plasm-server`** | In-process kernel, HTTP + MCP, Ratatui control station, embedded Postgres |
| **Remote client** | **`plasm`** | Strict transport-only terminal (`init` → `search` → `context` → `run`) |
| **Dev tooling** | **`plasm-repl`**, **`plasm-cgs`** | Local schema validation and wire debugging |

The **runtime** evaluates programs, caches cooperatively, enforces capability semantics, and records traces.

Session shaping (`intent`, reuse keys) matters when agents reconnect — see [MCP session reuse](reference/mcp-session-reuse.md).

---

## Federation (multiple catalogs in one session)

One logical session can load **multiple registry entries** (different APIs). Symbols stay session-local; dispatch resolves per owning graph—there is **no merged mega-schema**. See [Incremental teaching](reference/incremental-teaching-prompts.md#federated-sessions-multi-catalog).

---

## Where transport requirements belong

Wire details (pagination envelopes, auth headers) belong in **CML** and vendor `apis/<name>/` examples. Start from [Authoring reference — CML](authoring/reference.md) when debugging HTTP glue.

---

## Next steps

| If you want to… | Read |
|-----------------|------|
| Install and operate locally | [Appliance quick start](appliance/quickstart.md) |
| Connect agents via HTTP CLI | [Remote terminal (`plasm`)](reference/plasm-cgs-remote-terminal.md) |
| Author a new API catalog | [Catalog authoring tutorial](authoring/index.md) |
