# Plasm hash-chained evidence bundles

Plasm can emit a **hash-chained causal transcript** from agent intent through an approved [`PlasmComp`](plasm-language-definition.md#monadic-execution-contract-plasmcomp) to live HTTP effects. This complements [`TraceHub`](mcp-trace-correlation.md) observability with tamper-evident binding suitable for audit export.

Enable with:

```bash
export PLASM_EVIDENCE_CHAIN=1
```

Optional Ed25519 signature on the bundle head (32-byte seed hex):

```bash
export PLASM_EVIDENCE_SIGNING_KEY="<64-hex-char-seed>"
```

## Chain segments

Each segment hashes `SHA256(JCS({ schema_version: 2, seq, prev, kind }))` where **JCS** is [RFC 8785](https://www.rfc-editor.org/rfc/rfc8785) JSON Canonicalization. Genesis uses `prev: null`.

| `kind` | When emitted | Payload |
|--------|--------------|---------|
| `intent_bound` | Plan dry/live cycle start | `intent_digest`, `intent_len` |
| `comp_committed` | After dry-run validation | `plan_commit_id_hex`, `comp_semantic_sha256`, `step_topo` |
| `step_executed` | Each materialized plan step | `step_id`, fingerprints (opaque wire strings), `source_line`, `parsed_expr_digest` |
| `run_sealed` | Run snapshot archive | `run_id`, `run_bundle_digest` (matches `pr`+SHA256 preimage) |

Semantic comp commit uses the same canonical subset as [`plan_commit_id`](plasm-long-operations.md): `{ version, steps, bind, return }` (serde JSON; unchanged from dry-run).

Run snapshot preimages (`run_bundle_digest`, `RunArtifactId`) still use sorted serde JSON — not JCS — so existing `pr…` wire ids stay stable.

## Sidecar storage

When run snapshots are persisted (`PLASM_RUN_ARTIFACTS_DIR` or object store), evidence bundles are written as sidecars. Multiple return roots that share one transcript deduplicate bundle JSON by **chain head**:

```text
execute/{prompt_hash}/{session_id}/evidence/heads/{chain_head_hex}.evidence.json
execute/{prompt_hash}/{session_id}/evidence/run-heads/{run_id}.head   # pointer → head hex
```

Legacy per-run layout `{run_id}.evidence.json` may still be read when no head pointer exists.

**Retention:** object-store time-GC applies to execute run snapshot blobs only; paths under `…/evidence/` are excluded (same permanence class as code plans).

`RunArtifactDocument` v1 preimages are unchanged.

## Verification modes

| Mode | When | Checks |
|------|------|--------|
| **Emit** (`finish_bundle`) | Sidecar write | Last-segment head + step topo (incremental push trust) |
| **Serve** (HTTP GET, default CLI) | Read sidecar | Full chain walk + optional signature trust + step topo |
| **Serve + run seal** | HTTP GET when run artifact co-located, CLI `--artifact` | Serve checks + `run_sealed` digest vs artifact preimage |

Emit recomputes `run_sealed.run_bundle_digest` from artifact preimage and rejects wire `run_id` mismatches.

## HTTP

```http
GET /execute/{prompt_hash}/{session_id}/artifacts/{run_id}/evidence
```

Returns the JSON bundle; server verifies chain integrity (and optional signature trust policy) before respond.

## Verification (library / CLI)

```rust
use plasm_evidence::{DefaultChainVerifier, RunSealInputs, VerifyOptions};

DefaultChainVerifier::verify_with(&bundle, &VerifyOptions::default())?;
DefaultChainVerifier::verify_step_executed_topo(&bundle)?;
DefaultChainVerifier::verify_comp_commit_id(&bundle, &expected_commit_hex)?;
DefaultChainVerifier::verify_run_seal_with_inputs(&bundle, run_id, &inputs)?;
```

OSS remote terminal:

```bash
# Chain + step topo (+ optional signature trust policy)
plasm evidence verify ./execute/.../evidence/pr….evidence.json

# Full run_sealed digest cross-check (needs matching run snapshot + schema to re-parse)
plasm evidence verify ./bundle.json \
  --run-id pr… \
  --artifact ./execute/.../artifacts/pr… \
  --schema fixtures/schemas/plasm_language_matrix

# Rotation window: only accept signatures from listed pubkeys
export PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS="<old-pub-hex>,<new-pub-hex>"
plasm evidence verify ./bundle.json --trusted-pubkey <new-pub-hex>
```

When `PLASM_EVIDENCE_CHAIN=1`, plan dry/live paths emit sidecars for return-step run snapshots. Persisted backends use the head-dedup layout under `execute/{prompt_hash}/{session_id}/evidence/` (see **Sidecar storage** above). In-memory backends deduplicate bundle JSON by chain head when multiple return roots share one transcript.

## Ed25519 signing and key rotation

**Signing (emit):** set `PLASM_EVIDENCE_SIGNING_KEY` to 32-byte seed hex. Each bundle stores `signature.public_key_hex` + `signature.signature_hex` over the JCS preimage `{ schema_version: 2, scope, chain_head }`.

**Verification (consume):** by default, verify uses the public key embedded in the bundle. To enforce an allow-list during rotation, set:

```bash
export PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS="<pubkey-a-hex>,<pubkey-b-hex>"
```

Only signatures whose `public_key_hex` is in that CSV pass. CLI `--trusted-pubkey` flags merge with the env list.

**Rotation playbook:**

1. Generate a new 32-byte seed; derive pubkey (`signature.public_key_hex` from a signed test bundle).
2. Add **both** old and new pubkeys to `PLASM_EVIDENCE_TRUSTED_PUBLIC_KEYS` (overlap window).
3. Switch `PLASM_EVIDENCE_SIGNING_KEY` to the new seed on emitters.
4. After all in-flight bundles expire, drop the old pubkey from the trust list.

Old bundles remain verifiable indefinitely via their embedded pubkey unless a trust list is configured.

## Out of scope

- TraceHub row chaining
- Teaching TSV / full prompt bodies in the chain
- HTTP response bodies

Implementation: [`plasm-evidence`](https://github.com/PlasmTools/plasm-core/tree/main/crates/plasm-evidence/) (`jcs` module, RFC 8785 segment hashing), emitters in [`evidence_chain.rs`](https://github.com/PlasmTools/plasm-core/blob/main/crates/plasm-agent-core/src/evidence_chain.rs).
