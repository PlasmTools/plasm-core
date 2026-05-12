# Claude Code Instructions

Claude should follow the same Plasm authoring loop as Cursor and Codex. CGS / CML / catalog authoring doctrine is owned by `plasm-core` and lives in this submodule's `.cursor/skills/` tree.

Before creating or changing an API catalog, read:

- [AGENTS.md](AGENTS.md)
- [.cursor/skills/plasm-authoring/SKILL.md](.cursor/skills/plasm-authoring/SKILL.md)
- [.cursor/skills/plasm-authoring/reference.md](.cursor/skills/plasm-authoring/reference.md)
- [.cursor/agents/plasm-forge.md](.cursor/agents/plasm-forge.md)

Use these companion skills for follow-on work:

- [.cursor/skills/plasm-catalog-e2e-test/SKILL.md](.cursor/skills/plasm-catalog-e2e-test/SKILL.md) — Hermit then live / sandbox transport testing.
- [.cursor/skills/plasm-catalog-polish/SKILL.md](.cursor/skills/plasm-catalog-polish/SKILL.md) — autonomous diagnostic / fix loop.
- [.cursor/skills/plasm-catalog-score/SKILL.md](.cursor/skills/plasm-catalog-score/SKILL.md) — rubric scorecard.
- [.cursor/skills/plasm-catalog-reprint/SKILL.md](.cursor/skills/plasm-catalog-reprint/SKILL.md) — full-cutover regeneration.
- [.cursor/skills/plasm-catalog-retro/SKILL.md](.cursor/skills/plasm-catalog-retro/SKILL.md) — post-authoring retrospective.

## API Authoring Contract

Authoring `apis/<api>/domain.yaml` and `apis/<api>/mappings.yaml` is semi-autonomous:

- Use specs and docs as evidence.
- Design a relational CGS domain model.
- Write CML transport mappings.
- Validate with the compiler.
- Run the e2e-test ladder: Hermit against OpenAPI when available, then live API or vendor sandbox tests.
- Add `plasm-eval` cases for model conformance.
- Iterate until schema validation, transport checks, and eval coverage pass.

Do not mechanically convert an OpenAPI spec into one capability per endpoint. Compress the API into entities, relations, projections, scoped queries, and typed capabilities.

## Default Commands

```bash
cargo run -p plasm-cli --bin plasm -- schema validate apis/<api>
cargo run -p plasm-cli --bin plasm -- validate --schema apis/<api> --spec path/to/openapi.json
cargo run -p plasm-repl -- --schema apis/<api> --backend http://localhost:1080 --help
cargo run -p plasm-eval -- coverage --schema apis/<api> --cases apis/<api>/eval/cases.yaml
```

Hermit mock pass (Tier 1 of the e2e ladder):

```bash
hermit --specs path/to/openapi.json --port 9090 --use-examples
cargo run -p plasm-repl -- --schema apis/<api> --backend http://localhost:9090
```

If validation exposes a core language / runtime gap, report the gap instead of modifying core crates unless explicitly asked.
