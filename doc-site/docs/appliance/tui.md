# Control station (TUI)

**`plasm-server`** opens a Ratatui **control station** when **both** stdout and stdin are TTYs.

```bash
plasm-server          # interactive (default after install)
plasm-server --no-tui # headless — use CLI instead
plasm-server --tui    # force UI when auto-detection would skip it
```

Install and first boot: [Appliance quick start](quickstart.md). Operator guide (shortcuts, APIs, keys): this page. Non-interactive equivalent: [Appliance CLI reference](../reference/appliance-cli.md).

Press **`q`** to quit (graceful shutdown).

---

## Navigation

| Key | Action |
|-----|--------|
| **`Tab`** / **`Right`** | Next tab |
| **`Shift+Tab`** / **`Left`** | Previous tab |
| **`↑`** / **`↓`** or **`j`** / **`k`** | Move selection (list tabs) or scroll (Status, Clients, Logs) |
| **`PgUp`** / **`PgDn`** | Page scroll (Status, Clients, Logs) |
| **`g`** | Scroll to top (Status, Clients, Logs) |
| **`G`** | Scroll to bottom (Logs only) |
| **`Esc`** | Cancel modal / dismiss notice |
| **`q`** | Quit |

Tabs (left → right): **Status · Clients · APIs · OAuth · Keys · Runs · Storage · Logs**.

---

## First-time setup (typical flow)

After install, run **`plasm-server`**. The boot checklist runs embedded Postgres, loads compiled catalog artifacts, and opens MCP policy tables. When the **Status** tab shows listeners and Postgres as ready:

1. **APIs** — enable the catalogs agents may call.
2. **Keys** — create at least one **MCP transport** API key.
3. **Clients** — copy MCP or **`plasm`** client config (uses the key selected on **Keys**).
4. **APIs** (again, if needed) — store **outbound** vendor credentials (GitHub PAT, etc.) or use **OAuth**.

Until step 2 completes, **Clients** copy actions warn that no transport key exists.

---

## Enable APIs (APIs tab)

The **APIs** tab lists every loaded registry **`entry_id`**. Enabled catalogs are what the appliance tenant exposes to MCP and HTTP execute.

| Key | Action |
|-----|--------|
| **`/`** | Open filter bar — type to narrow the list; **`Esc`** clears |
| **`↑`** / **`↓`** or **`j`** / **`k`** | Select a catalog row |
| **`Space`** | Toggle enabled/disabled for the selected catalog (staged locally) |
| **`s`** | **Save** the staged allowlist to Postgres |

**Workflow**

1. Go to **APIs**.
2. Press **`/`**, type e.g. `github`, **`Esc`** when done filtering.
3. Move to a row with **`j`**/**`k`**, press **`Space`** to enable (row shows on/off state).
4. Repeat for each catalog you need.
5. Press **`s`**. Wait for the footer notice — admin jobs run asynchronously; do not spam **`s`** while **Busy** appears.

The detail pane on the right shows auth support (**public**, **API key**, **OAuth**) for the selected catalog.

### Outbound credentials (same tab)

These are **vendor** credentials (plane 3 in [MCP & credentials](onboarding.md)), not MCP transport keys.

| Key | Action |
|-----|--------|
| **`a`** | Set an **outbound API key** for the selected catalog (when the catalog declares a `hosted_kv` slot) |
| **`o`** | Jump to **OAuth** tab for the selected catalog |

For **`a`**: type the secret, **`Enter`** saves to local auth KV, **`Esc`** cancels. Catalogs that only declare `env:` auth need the variable set in your shell instead — the TUI explains when no `hosted_kv` slot exists.

---

## MCP transport keys (Keys tab)

**MCP transport keys** authenticate callers to **`/mcp`** (Cursor, other MCP clients, and optionally **`plasm init --api-key`**). They are separate from outbound GitHub/Google credentials.

| Key | Action |
|-----|--------|
| **`↑`** / **`↓`** or **`j`** / **`k`** | Select a key row |
| **`a`** | **Add** key — type a label, **`Enter`** confirms, **`Esc`** cancels |
| **`r`** | **Rotate** selected key (new secret; old invalidated) |
| **`d`** | **Revoke** — then **`y`** confirms, **`Esc`** cancels |
| **`c`** | **Copy secret** to clipboard (one-time reveal via admin job) |
| **`#`** | Copy key label line |

**Workflow — first key**

1. Go to **Keys**.
2. Press **`a`**.
3. Type a label (e.g. `cursor`), press **`Enter`**.
4. When the job completes, press **`c`** to copy the Bearer secret. Store it somewhere safe — treat it like a password.

**Rotate or revoke**

- Select the row, press **`r`** to rotate (clients must update Bearer token).
- Press **`d`**, then **`y`** to revoke.

The selected key row is remembered when you switch tabs — **Clients** uses this selection for copy actions.

---

## Connect a client (Clients tab)

Shows MCP JSON and **`plasm`** profile templates for the **currently selected key** (from **Keys**).

| Key | Action |
|-----|--------|
| **`c`** | Copy **MCP client config** (`streamableHttp` + `Authorization: Bearer …`) |
| **`p`** | Copy **`plasm`** CLI profile JSON (`server` + `api_key`) |
| **`#`** | Copy MCP URL only (`http://127.0.0.1:<port>/mcp`) |
| **`↑`** / **`↓`** | Scroll the preview panel |

**Cursor (Streamable HTTP)**

1. Create a key on **Keys** (above).
2. Open **Clients**, press **`c`**.
3. Paste into your MCP client config. The copied JSON uses the live listener port from **Status**.

**`plasm` remote terminal**

1. Same key selection on **Keys**.
2. **Clients** → **`p`** copies a profile snippet, or run manually:

```bash
plasm init --server http://127.0.0.1:<port> --api-key "<paste-secret>"
```

Use the **Status** tab bind address (`127.0.0.1:<port>` or the “local clients” line when listening on all interfaces).

See [Remote terminal (`plasm`)](../reference/plasm-cgs-remote-terminal.md).

---

## OAuth (OAuth tab)

For catalogs that require OAuth delegation instead of a static PAT:

| Key | Action |
|-----|--------|
| **`n`** | New provider wizard (upsert `oauth_provider_apps`) |
| **`d`** | Device authorization bind (RFC 8628 — blocking poll) |
| **`x`** then **`y`** | Disable selected provider |

Credential-plane background and Google Workspace friction: [MCP & credentials](onboarding.md).

From **APIs**, **`o`** on a catalog jumps here with that catalog pre-selected.

---

## Other tabs

| Tab | Use |
|-----|-----|
| **Status** | HTTP/MCP URL, Postgres, catalog directory, boot milestones |
| **Runs** | Recent execute runs and artifact links |
| **Storage** | `PLASM_TRACE_ARCHIVE_DIR`, `PLASM_RUN_ARTIFACTS_DIR` paths |
| **Logs** | In-process log tail; optional mirror file via **`PLASM_APPLIANCE_DIAG_LOG`** |

---

## TUI vs CLI

| Task | TUI | CLI |
|------|-----|-----|
| Enable APIs | **APIs** → **`Space`** → **`s`** | `plasm-server mcp apis set …` |
| MCP transport key | **Keys** → **`a`** | `plasm-server mcp keys add --name …` |
| Copy MCP config | **Clients** → **`c`** | `plasm-server mcp keys reveal …` |
| Outbound API key | **APIs** → **`a`** | env / OAuth CLI |
| OAuth provider | **OAuth** → **`n`** / **`d`** | `plasm-server oauth …` |
| Scripting / CI | — | **`--no-tui`** + [CLI reference](../reference/appliance-cli.md) |

---

## Diagnostics

| Variable | Role |
|----------|------|
| **`PLASM_APPLIANCE_DIAG_LOG`** | Append tracing lines to a file |
| **`PLASM_APPLIANCE_BOOT_TRACE_STDERR`** | Mirror bootstrap phases to stderr during TUI boot |

Do **not** place the diag log inside `{data-dir}/postgres/`.

Maintainer matrix: [Surface inventory](../reference/appliance-surface-inventory.md).
