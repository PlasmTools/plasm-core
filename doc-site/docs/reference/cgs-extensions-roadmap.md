# CGS extensions roadmap (working list)

Open design notes for evolving **Capability Graph Schema** and adjacent authoring surfaces. Items are **not committed work**—prioritize by eval quality, authoring burden, and implementation cost.

---

## 1. Canned DOMAIN / prompt examples per parameter or capability (high signal)

**Problem:** DOMAIN lines and symbol-tuned examples are synthesized in [`plasm-oss/crates/plasm-core/src/prompt_render.rs`](plasm-oss/crates/plasm-core/src/prompt_render.rs). String parameters always use a literal `"example"` in `invoke_dotted_call_arg_example` (see `FieldType::String` branch). That is correct mechanically but weak pedagogy—e.g. `calculate` would read better as:

```text
e6.m1(p8="1.5 + 2 * 3")  ;;  Evaluate a safe arithmetic expression
```

than `p8="example"`.

**Directions:**

| Approach | Pros | Cons |
|----------|------|------|
| **A. Optional `example:` on capability parameters** in `domain.yaml` (per parameter) | Precise per-field; validates as normal string | Verbose for large APIs |
| **B. Optional `prompt_examples:` on the capability** | One blob per operation | Less granular |
| **C. Semantic tags** (e.g. `string_semantics: arithmetic_expression`) with a small registry of built-in example generators | DRY across schemas | Registry maintenance; may not fit every API |

**Constraints to preserve:**

- Examples must still **parse and type-check** through `domain_line_valid` / witness validation.
- Symbol expansion (`p#`) must remain consistent—examples are **values**, not alternate symbol names.

**Todo (implementation when prioritized):**

- [ ] Decide schema shape (A/B/C or hybrid: optional `example` override + fallback to current heuristics).
- [ ] Thread through loader → `InputFieldSchema` (or capability-level map).
- [ ] Replace or augment `invoke_dotted_call_arg_example` / query example helpers to use authored examples when present.
- [ ] Add a τ² retail fixture case (`calculate`) as regression for prompt snapshots / tests.

---

## 2. Other CGS / prompt extensions (backlog)

Short ideas worth tracking; expand into separate sections when a line of work starts.

- [ ] **Response-shape hints for non-GET writes** — richer `provides` / decode hints where create/update returns domain-specific projections (partially covered today by `provides:`).
- [ ] **Per-capability DOMAIN inclusion toggles** — rare cases where an operation is valid but should not appear in the default prompt (with explicit `exclude_from_domain` or similar), if ever needed for size control.
- [ ] **Multi-example sets** — rotate or A/B several canonical lines per capability for few-shot diversity (heavier prompt cost).
- [ ] **Cross-schema example libraries** — reusable YAML snippets for common patterns (pagination, OAuth) without duplicating prose.

---

## 3. How this ties to authoring

When extending CGS, keep **domain.yaml** as semantic truth and **mappings.yaml** as transport; prompt-only data should stay **optional** and **downgrade gracefully** to current behavior when absent (per [plasm forge skill](../.cursor/skills/plasm-forge/SKILL.md)).

---

*Last updated: ad-hoc; bump when items graduate to implementation or are rejected.*
