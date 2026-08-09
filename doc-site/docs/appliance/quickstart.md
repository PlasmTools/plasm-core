# Appliance quick start

**Outcome:** Install **`plasm-server`**, boot the control station (or headless mode), enable catalog APIs, provision an MCP key, and connect a client — then optionally use the **`plasm`** remote terminal against the same HTTP listener.

---

## Install

From [plasm.tools/get](https://plasm.tools/get/):

```bash
curl -fsSL https://plasm.tools/install/install.sh | bash
```

Appliance-only install:

```bash
curl -fsSL https://plasm.tools/install/install.sh | bash -s -- --product appliance
```

Default layout: **`~/.plasm/appliance/`** (`postgres/`, `local/`, `catalogs/`). After install, **`plasm-server`** picks up `{appliance}/catalogs` automatically.

From source (plasm-core checkout):

```bash
cargo build -p plasm-server --release
cargo run -p plasm --bin plasm-pack-catalogs -- \
  --workspace . --apis-root apis --output-dir target/plasm-catalogs
cargo run -p plasm-server --release -- --catalog-dir target/plasm-catalogs
```

---

## First boot

Interactive terminal (TTY on stdin and stdout):

```bash
plasm-server
```

**Verify:** Ratatui **control station** opens (Status tab shows HTTP/MCP listeners and Postgres).

Headless (CI, scripts, or non-TTY):

```bash
plasm-server --no-tui
```

**Verify:** stderr bootstrap milestones; `curl -sS http://127.0.0.1:3000/v1/health` → `{"status":"ok"}`.

Use **`--data-dir`** or **`PLASM_APPLIANCE_DIR`** only when you need a non-default state root. See [Surface inventory](../reference/appliance-surface-inventory.md) for env overrides.

---

## Enable APIs and MCP key

Use the **control station** (recommended):

1. **APIs** tab — filter (`/`), toggle catalogs (`Space`), save (`s`).
2. **Keys** tab — **`a`** add a label, **`Enter`**, then **`c`** copy the Bearer secret.
3. **Clients** tab — **`c`** copy Cursor MCP JSON (or **`p`** for a **`plasm`** profile).

Step-by-step with every shortcut: [Control station (TUI)](tui.md).

**CLI equivalent:**

```bash
plasm-server mcp migrate-db   # only if bootstrap reported migrate errors
plasm-server mcp apis set github pokeapi
plasm-server mcp keys add --name cursor
```

**Verify:** MCP client connects with `Authorization: Bearer <api_key>` to `/mcp` (Streamable HTTP).

Credential planes (transport vs outbound): [MCP & credentials](onboarding.md). CLI tables: [CLI reference](../reference/appliance-cli.md).

---

## Connect Cursor (example)

The **Clients** tab emits JSON like:

```json
{
  "mcpServers": {
    "plasm": {
      "url": "http://127.0.0.1:3000/mcp",
      "headers": {
        "Authorization": "Bearer <your-api-key>"
      }
    }
  }
}
```

Use **`streamableHttp`** transport per your MCP client’s schema.

---

## Remote terminal against the appliance

In a project directory:

```bash
plasm init --server http://127.0.0.1:3000 --api-key "$PLASM_API_KEY"
plasm search "pokeapi pokemon"
plasm context -i "inspect pokemon" pokeapi:Pokemon
echo 'e1(name=pikachu)[name,id]' | plasm run
```

Full flow: [Remote terminal (`plasm`)](../reference/plasm-cgs-remote-terminal.md).

---

## Next steps

| Goal | Page |
|------|------|
| Install, TUI walkthrough (APIs, keys, clients) | [Control station (TUI)](tui.md) |
| OAuth vs PAT, credential planes | [MCP & credentials](onboarding.md) |
| `plasm-server mcp` / `oauth` commands | [CLI reference](../reference/appliance-cli.md) |
| Author or pack catalogs | [Catalog authoring](../authoring/index.md) |
