# Catalog CBOR IL pipeline

**Architecture context:** `saas-architecture.md` (catalog modes, auth boundaries).

This document describes how authored **`apis/<name>/`** catalogs are packed into **portable CBOR IL artifacts**, loaded at runtime via **`--catalog-dir`**, and hot-reloaded without rebuilding the executor.

## Artifacts

Each packed catalog produces two files under the catalog directory:

| File | Purpose |
|------|---------|
| `<entry_id>.v<version>.<hash12>.cgs.cbor` | CBOR-encoded [`CGS`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs) interchange (`PLASM_CATALOG_FORMAT_VERSION = 1`) |
| `<entry_id>.v<version>.<hash12>.manifest.json` | [`CatalogManifest`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/catalog_il.rs): `entry_id`, `version`, full `cgs_hash`, artifact filename |

- **`cgs_hash`**: SHA-256 hex of canonical JSON ([`CGS::catalog_cgs_hash_hex`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/schema.rs)); verified after CBOR decode.
- **No `target_triple`**: pack once, run on any supported host triple.
- **Version pick**: highest `CGS.version` per `entry_id` when scanning a directory.

Implementation: [`catalog_il.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/catalog_il.rs), [`catalog_data.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/catalog_data.rs).

## Compile dispatch

Compile and projection hydration always use in-tree [`plasm_compile::compile_operation`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-compile/src/lib.rs) / [`compile_query`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-compile/src/lib.rs). There is **no** native dylib compile override path.

## Agent startup: build (`apis/`) vs runtime (`--catalog-dir`)

| Phase | Tool | What happens |
|------|------|----------------|
| **Authoring** | Edit **`apis/<name>/domain.yaml`** + **`mappings.yaml`** | Source of truth in git. |
| **Pack (build)** | **`plasm-pack-catalogs`** ([`plasm_pack_catalogs.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm/src/bin/plasm_pack_catalogs.rs)) | Emits CBOR IL + manifest per package. Default output: **`target/plasm-catalogs`**. |
| **Runtime** | **`plasm-mcp --catalog-dir <dir>`** | [`load_registry_from_catalog_dir`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/catalog_data.rs) — highest **`version` per `entry_id`**. |
| **Single schema** | **`--schema <path>`** | One CGS (no catalog dir). |

**Mutual exclusion:** do not combine **`--catalog-dir`** with **`--schema`**.

[`catalog_data`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/catalog_data.rs) builds an [`InMemoryCgsRegistry`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-core/src/discovery.rs) and runs template validation across entries.

## Session reuse and pinning

[`SessionReuseKey`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/execute_session.rs) includes **`catalog_cgs_hash`** (and [`ExecuteSession`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/execute_session.rs) stores it) so HTTP/MCP reuse paths do not silently reuse a session after the pinned CGS for that entry changes.

## Build

```bash
# Pack all apis/ (or subset via --package-list)
cargo run -p plasm --bin plasm-pack-catalogs -- \
  --workspace . --apis-root apis --output-dir target/plasm-catalogs

# Local dev shortcut (plasm monorepo justfile)
just build-catalogs
```

## HTTP / MCP examples

**Catalog from packed CBOR IL** (no `--schema`):

```bash
cargo run -p plasm-mcp-app --bin plasm-mcp-saas -- \
  --catalog-dir target/plasm-catalogs --http --mcp --port 3000
```

For OSS data-plane-only HTTP, use `-p plasm` instead of `-p plasm-mcp-app`.

**Appliance:**

```bash
cargo run -p plasm --bin plasm-pack-catalogs -- \
  --workspace . --apis-root apis --output-dir target/plasm-catalogs
cargo run -p plasm-server --release -- --catalog-dir target/plasm-catalogs
```

## Kubernetes / Helm

The `plasm-mcp` chart accepts **`--catalog-dir`** with a volume of `*.cgs.cbor` + `.manifest.json` files. Default images ship **`--catalog-dir /app/catalogs`** (CBOR IL produced at Docker build from repo `apis/`).

**Hot reload:** **`pluginHotReload`** (Helm value name; writable catalog volume, sidecar polling bundle digest, **`POST /internal/catalog-registry/v1/reload`**). See monorepo `deploy/docs/catalog-hot-reload-k8s.md`.

**Reload endpoint:** `POST /internal/catalog-registry/v1/reload` with **`x-plasm-control-plane-secret`**. Returns **`409`** if started with **`--schema`**. Hosted implementation: `http_catalog_registry.rs` in the private `plasm` monorepo (`plasm-saas`).

## Execute run artifacts (snapshots)

[`RunArtifactStore`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/run_artifacts.rs) backs **`GET /execute/.../artifacts/:run_id`** and MCP **`resources/read`**.

| Mode | Configuration |
|------|----------------|
| **In-memory** (default) | No env; snapshots are process-local. |
| **Object store** | **`PLASM_RUN_ARTIFACTS_URL`**: `object_store` URL (`s3://`, `file://`, …). |
| **Time-based GC** | **`PLASM_RUN_ARTIFACTS_RETENTION_SECS`** (default 7d), **`PLASM_RUN_ARTIFACTS_GC_INTERVAL_SECS`** (default 300s). |

## Persistent session graph cache (delta + snapshot)

| Mode | Configuration |
|------|----------------|
| **Disabled** (default) | In-memory session graph only. |
| **Object store** | **`PLASM_GRAPH_CACHE_URL`**. Hot RAM cap: **`PLASM_GRAPH_HOT_MAX_ENTITIES`** (default **2048** when persistence is active). |

See [CLI & env index](cli-and-env.md) and [Runtime schema overlay](schema-overlay.md).
