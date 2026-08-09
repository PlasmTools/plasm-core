# Vultr public HTTP API v2

Plasm CGS slice for the [Vultr customer API (v2)](https://www.vultr.com/api/): `domain.yaml` and `mappings.yaml` for agent (`plasm-cgs`, `plasm-mcp` plugins) and validation.

**Scope in this tree (phases 1–5+)** — through **P4+**: account and billing, catalogs, **P2** identity, **P3** VPC / compute / LB / firewalls, **P4+** storage, DNS, object storage, and managed DB. **P5** adds **VKE** (`KubernetesCluster`: list, get, create, update, delete; `VkeKubeconfig` get for the kubectl file), **bare metal** (`BareMetal` — separate from **BareMetalPlan**: CRUD, start, reboot, halt), and **CDN** pull and push **zones** (`CdnPullZone` / `CdnPushZone`, including pull purge and push files + presigned upload in later passes). **P5b** covers **VKE** node pools, labels, taints, node lifecycle, `KubernetesVersion`, and extra bare metal (reinstall, VNC, modern VPC attach/detach). **Schema v16 (current file version)** adds a **stricter field-typing tranche (non-date)** on top of **v15**: `Vpc.region` and **`select`** / `allowed_values` where the v2 doc lists a closed set—**`ReservedIp.ip_type`**, **`BlockStorage` `status` / `block_type`**, **`FirewallRule` `kind` (wire `type`) / `ip_type` / `protocol`**, **`StartupScript` `type`** with **`script` as `blob`** (Base64 on the wire), **CDN** zone `status` and pull **`origin_scheme`**, **`DnsRecord` `type`**, **`ManagedDatabase` `database_engine_version` as `integer`**, **`maintenance_dow`** weekday enums—plus matching **capability parameters** for creates where applicable. **v15** remains: reverse **`query_scoped`** **`Region.instances`** / **`FirewallGroup.instances`**, v14 tranche (dates, `entity_ref`, VKE, `materialize`, etc.). The v2 public doc’s `/v2/cdns/...` tree in this repository is complete for pull zones, push zones, and push files; other product areas of the Vultr API are only included where listed above.

**OpenAPI:** use the *Download OpenAPI specification* from the v2 doc page, save it locally (e.g. `apis/vultr/openapi.json`), and use it for manual diffing and optional `plasm validate` when a machine-readable spec is present. This repo does not ship a pinned OpenAPI file.

## Authentication

Bearer API key. Set:

```bash
export VULTR_API_KEY='…'
```

The schema declares `auth.scheme: bearer_token` with `env: VULTR_API_KEY`.

## Backend

Default origin is `https://api.vultr.com` (`http_backend` in `domain.yaml`); paths include the `/v2/…` prefix in CML. Override for testing:

```bash
--backend https://api.vultr.com
```

## Examples

List regions and compute plans (cursor pagination: `--limit`, `--all`, optional `--cursor`):

```bash
cargo run -p plasm-repl -- --schema apis/vultr --backend https://api.vultr.com
  region query --limit 50

cargo run -p plasm-repl -- --schema apis/vultr --backend https://api.vultr.com
  plan query --type vhf --limit 20
```

Current account (GET `/v2/account`); the row key is the account `email` from the response, so the positional id must match that email:

```bash
# replace with your account email as returned by the API
export ACCT_EMAIL='admin@example.com'
cargo run -p plasm-repl -- --schema apis/vultr --backend https://api.vultr.com
  account "$ACCT_EMAIL"
```

`AccountBgp` and `AccountBandwidth` are singleton reads: `implicit_request_identity` is set, so you pass any stable positional id for the cache (for example `current`):

```bash
cargo run -p plasm-repl -- --schema apis/vultr --backend https://api.vultr.com
  accountbgp current
cargo run -p plasm-repl -- --schema apis/vultr --backend https://api.vultr.com
  accountbandwidth current
```

Identity and network (examples; destructive deletes omitted):

```bash
# List all API keys (unfiltered, no server pagination in mapping)
cargo run -p plasm-repl -- --schema apis/vultr --backend https://api.vultr.com
  apikey query

# Team users, IAM read models, SSH keys, VPCs, firewalls
cargo run -p plasm-repl -- --schema apis/vultr --backend https://api.vultr.com
  user query --limit 20
cargo run -p plasm-repl -- --schema apis/vultr --backend https://api.vultr.com
  iampolicy query --limit 10
cargo run -p plasm-repl -- --schema apis/vultr --backend https://api.vultr.com
  vpc query --limit 5
```

Create flows use the usual `create` / `update` / `delete` subcommands; see `plasm-cgs <entity> create --help` (e.g. `user create`, `vpc create`, `sshkey create` — SSH body maps `key_material` to the wire `ssh_key` field).

## Packaging

`vultr` is listed in [`deploy/saas-packaged-apis.txt`](../../../deploy/saas-packaged-apis.txt) and [`plasm-oss/scripts/oss-packaged-apis.txt`](../../scripts/oss-packaged-apis.txt) for production images and OSS appliance tarballs.