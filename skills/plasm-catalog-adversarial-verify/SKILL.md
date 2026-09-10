---
name: plasm-catalog-adversarial-verify
description: Programming-agent adversarial prove loop for Plasm catalogs (AppWorld-first). Certifies teaching + dry/live Plasm and mandatory semantic auto-seed (intent-only plasm_context). Writes evidence under apis/.../e2e/. Use when verifying or hardening a catalog outside CUGA/eval ladders.
---

# Plasm catalog adversarial verify

Programming agent certifies **catalog machine-spirits** (CGS/CML + teaching + discovery seating). This is **outside** CUGA / ladder harnesses — do not treat ladder pass rates as catalog certification.

## Mandatory tiers (full cutover)

Complete **all** tiers. Do not stop after schema validate or seeded HTTP execute.

### 1. Schema / cardinality

- `plasm-cgs schema validate` on the catalog (hard-fail dual-wire pagination, missing blocks).
- List cardinality: ≤1 `query` + ≤1 `search` per entity (no twin-query heresy).

### 2. Teaching + dry/live (seeded or execute open)

- Open an execute / MCP session that seats the entities under test.
- Audit the teaching card: auth gates, shelf discriminants (e.g. LikedSong vs Song), critical mutators/polarities expressible.
- Dry (`plasm` plan / `?mode=plan`) then live a minimal program against Hermit or the real backend.

### 3. Semantic auto-seed (mandatory)

Product path — **not optional**, not a harness shim:

1. Start `plasm-mcp` (or hosted equivalent) with catalogs packed and **`PLASM_DISCOVERY_SEMANTIC_AUTO_SEED=1`** plus **`OPENROUTER_API_KEY`** (same env family as CUGA treatment / intent-discovery docs).
2. Confirm auto-seed host shape: `tools/list` exposes `plasm_context` / `plasm` / `plasm_run` and **omits** `discover_capabilities`; `plasm_context` **`session_mode: "new"` rejects `seeds`**.
3. Call **`plasm_context`** with **`session_mode: "new"` + `intent` only** (no seeds) for representative intents mined from `eval/cases.yaml` and ladder-relevant scenarios.
4. Assert:
   - Teaching card seats the right entities (auth co-seed, shelf discriminants, federated supervisor passwords when AppWorld).
   - No `hard_miss` / unjustified `clarify` / brand-lock routing errors.
   - Critical mutators/polarities remain expressible on the card.
5. Dry (then live when backend is up) a **minimal program from that card** using taught `e#` forms — not hand-invented seeds.

**Fix CGS/discovery when seating is wrong** (`discovery.names`, descriptions, `seed_class`, `co_seed_with`, shelf copy). Do **not** paper over with harness hacks, forced seeds, or prompt-only patches.

Brand-lock / named-catalog matching must be **whole-token** (short ids like `phone` must not fire inside `headphones`).

### 4. Evidence

Per API, write/update `apis/<api>/e2e/adversarial-verify-<date>.md` including a **Semantic seed** section:

- pass/fail
- intent used
- what was seated (`e#` → entity)
- dry verdict / program used

Roll up multi-API work in `apis/<family>/e2e/adversarial-roll-<date>.md` with a **Semantic seed** column.

## Anti-patterns

- Certifying from manual `{api, entity}` seeds only.
- Skipping semantic auto-seed because “schema validate passed”.
- Teaching non-executable sugar (`[…]` legacy projections) without noting runtime rejection.
- Dual-path shims “for later” — full cutover.

## Related

- Authoring core: [plasm-authoring](../plasm-authoring/SKILL.md)
- Intent discovery / auto-seed: repo `docs/intent-discovery.md`
- E2E transport loop: [plasm-catalog-e2e-test](../plasm-catalog-e2e-test/SKILL.md)
