# Start here

**Outcome:** You will (1) run a released **`plasm-server`** appliance or (2) from a **[plasm-core](https://github.com/PlasmTools/plasm-core)** checkout validate a catalog and run one REPL query — then pick your next path.

---

## Track A — Released appliance (recommended)

1. **Install** — [plasm.tools/get](https://plasm.tools/get/) → **`plasm-server`**
2. **Boot** — TTY → control station; scripts → **`--no-tui`**
3. **Enable APIs + MCP key** — control station **APIs** / **Keys** tabs ([TUI guide](appliance/tui.md)) or `plasm-server mcp …`
4. **Client** — `plasm init` → `plasm context` → `plasm run` against the HTTP listener

Full walkthrough: [Appliance quick start](appliance/quickstart.md).

---

## Track B — From source (catalog authors)

**Prerequisites:** Rust (`cargo`), repo cloned with `apis/` populated.

### Step 1 — Validate a catalog

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/dnd5e
```

**Verify:** Exit code `0`. The argument is the **catalog directory** (`apis/<name>/`), not `domain.yaml` alone.

### Step 2 — REPL: one live read

```bash
cargo run -p plasm-repl -- \
  --schema apis/dnd5e \
  --backend https://www.dnd5eapi.co
```

**Verify:** Decoded rows at the `repl>` prompt (no transport errors).

### Step 3 — Schema CLI smoke (`plasm-cgs`)

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/pokeapi
```

### Step 4 — Pack catalogs and run the appliance

```bash
cargo run -p plasm --bin plasm-pack-catalogs -- \
  --workspace . --apis-root apis --output-dir target/plasm-catalogs

cargo run -p plasm-server --release -- --catalog-dir target/plasm-catalogs
```

**Verify:**

| Check | Command |
|-------|---------|
| Health | `curl -sS http://127.0.0.1:3000/v1/health` |
| Registry | `curl -sS http://127.0.0.1:3000/v1/registry` lists multiple `entry_id`s |

MCP Streamable HTTP is on the same host at **`/mcp`** (see [Appliance quick start](appliance/quickstart.md) for Bearer keys).

---

## Choose your next path

| Goal | Go to |
|------|------|
| Install, TUI, MCP keys | [Appliance quick start](appliance/quickstart.md) |
| `plasm init` / remote terminal | [Remote terminal (`plasm`)](reference/plasm-cgs-remote-terminal.md) |
| Credentials, OAuth vs PAT | [MCP & credentials](appliance/onboarding.md) |
| Mental model | [Concepts](concepts.md) |
| Author catalogs | [Catalog authoring](authoring/index.md) |
| Optional incoming JWT | [Incoming auth](reference/plasm-mcp-incoming-auth.md) |
