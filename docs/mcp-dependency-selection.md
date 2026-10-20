# MCP dependency selection — 2026-10-18

## Recommendation

Use the official `rmcp 3.5.1`, pinned for the migration, with default features
disabled and only the required HTTP client/server features enabled. Replace the
existing SDK boundary deliberately; do not maintain parallel fallback clients.
The newly copied, unmodified transport 0.9.1 source was moved out of the source
tree into the isolated probe directory. The subsequent authorized migration
has moved both the outbound runtime and inbound server to rmcp and updated both
workspace manifests and lockfiles. The old SDK/schema vendors are removed; see
[cutover status](mcp-sdk-cutover.md).

## Released options

| Candidate | Published support | Assessment |
|---|---|---|
| Official `rmcp 3.5.1` | MCP 2026-07-28 and earlier versions; Rust 1.88 minimum | Recommended. Independently exercised below. Explicit initialize or discover lifecycle modes; no stdio needed. |
| `rust-mcp-sdk 2.0.0` | MCP 2026-07-28 stateless; 1.x is its separate 2025 LTS line | Viable upstream alternative, but not locally protocol-tested in this investigation. It is a major migration too, not an upgrade of the existing parser alone. |
| Current patched SDK 0.9.0 / transport 0.9.1 | Existing Plasm boundary | Reproduced legal SSE field rejection. Do not extend this fork as the preferred solution. |

Sources checked: [official release 3.5.1](https://github.com/modelcontextprotocol/rust-sdk/releases/tag/rmcp-v3.5.1),
[official SDK README](https://github.com/modelcontextprotocol/rust-sdk/blob/main/README.md),
[conformance roadmap](https://github.com/modelcontextprotocol/rust-sdk/blob/main/ROADMAP.md),
[alternative SDK version matrix](https://github.com/rust-mcp-stack/rust-mcp-sdk#readme).
Crates.io API checks confirmed both versions are published and not yanked.
The official roadmap reports full date-versioned conformance; that upstream
claim is distinct from our bounded local probes.

## Initial selection probes (before cutover)

Isolated workspace: `target/mcp-dependency-probe/` at the monorepo root.
It has its own Cargo manifest and lockfile, using exact `rmcp = 3.5.1`.

| Exchange | Result |
|---|---|
| Independent TCP peer with `event:message` / `data:{...}`, current client | FAIL, initialization; minimal regression in `mcp_http_protocol` |
| Independent HTTP peer with the same legal no-space SSE, rmcp | PASS initialize and tools/list |
| Independent 2026 peer, explicit Discover lifecycle | PASS server/discover and tools/list; no initialize fallback; protocolVersion/clientInfo/clientCapabilities present in per-request metadata |
| Actual Plasm stateful HTTP listener, rmcp | PASS initialization and tools/list, three Plasm tools returned |
| Actual Plasm stateless 2026 listener, rmcp | FAIL: Plasm discovery response is not a valid DiscoverResult |

The last failure exposes a host contract gap: Plasm's handwritten
`stateless/discover.rs` response lacks `resultType`, `ttlMs`, and `cacheScope`.
The candidate correctly accepts the independent response containing those
fields. This is not evidence that the candidate lacks 2026 support.

Commands:

```sh
cargo run --manifest-path target/mcp-dependency-probe/Cargo.toml

PLASM_MONTY_BINARY=/absolute/path/to/vendored/monty \
PLASM_MCP_CLIENT_PROBE_BINARY=/absolute/path/to/target/mcp-dependency-probe/target/debug/mcp-dependency-probe \
  cargo test -p plasm-agent-core --test mcp_python_protocol python_review \
  --offline -- --nocapture
```

The second command initially failed at the stateless host gap. Local
logs are `/tmp/rmcp-probe.log` and `/tmp/rmcp-plasm-probe.log`. No model calls,
vendor API calls, databases or production mutations were used in these probes.
After cutover, both stateful and stateless official-client probes pass against
Plasm: the native SDK now owns discovery envelopes. Python review, execution,
artifact reads and isolated pgvector discovery also pass; see the integration report.

## Migration requirements

1. Replace outbound SDK use behind `McpSession`; retain typed protocol errors,
   full tool envelopes, deadlines, task-feature rejection and exactly-once
   dispatch accounting. Choose protocol mode explicitly.
2. Disable automatic session-expiry POST replay (`reinit_on_expired_session`)
   and assess SSE resumption separately. The new library exposes these policies;
   accepting its defaults blindly would weaken Plasm's no-replay contract.
3. Move inbound HTTP/MCP and protocol models to the same SDK, including correct
   2026 discovery and list metadata. Preserve auth, resources, notifications,
   logical sessions and trace correlation. Remove superseded handwritten
   protocol contracts and old SDK/schema patches as their callers are replaced.
4. Run independent protocol tests, SDK-to-Plasm tests for both explicit protocol
   modes, Python/discovery integration and auth/session regressions. Extend the
   probe to tools/call and full envelope/error cases before declaring migration
   complete.

The original SDK references spanned 38 Rust files: 35 in agent-core, two in
runtime and one in SaaS. This was a protocol-boundary migration, not a Cargo
version edit. Bounded local verification is recorded in the cutover report;
it does not establish interoperability with every upstream server.
