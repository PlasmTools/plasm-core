# Agent Instructions

This repository contains the OSS Plasm compiler/runtime workspace and curated API catalogs under `apis/`.

## API Authoring

`plasm-core` owns CGS / CML / catalog authoring doctrine. When creating or editing an API catalog, follow the local skill suite under `.cursor/skills/`:

- [.cursor/skills/plasm-authoring/SKILL.md](.cursor/skills/plasm-authoring/SKILL.md) — primary workflow (read spec → model → map → validate → e2e test → eval).
- [.cursor/skills/plasm-authoring/reference.md](.cursor/skills/plasm-authoring/reference.md) — deep CGS / CML reference.
- [.cursor/skills/plasm-catalog-e2e-test/SKILL.md](.cursor/skills/plasm-catalog-e2e-test/SKILL.md) — Hermit, live, and sandbox transport testing.
- [.cursor/skills/plasm-catalog-polish/SKILL.md](.cursor/skills/plasm-catalog-polish/SKILL.md) — autonomous diagnostic / fix loop.
- [.cursor/skills/plasm-catalog-score/SKILL.md](.cursor/skills/plasm-catalog-score/SKILL.md) — rubric scorecard.
- [.cursor/skills/plasm-catalog-reprint/SKILL.md](.cursor/skills/plasm-catalog-reprint/SKILL.md) — full-cutover regeneration of weak catalogs.
- [.cursor/skills/plasm-catalog-retro/SKILL.md](.cursor/skills/plasm-catalog-retro/SKILL.md) — post-authoring retrospective.
- [.cursor/agents/plasm-forge.md](.cursor/agents/plasm-forge.md) — Cursor agent (**Plasm forge**) that drives the loop autonomously.

API authoring is semi-autonomous. Agents may read specs, design entities, edit YAML, run validation, test against mocks and sandboxes, and add eval cases, but `domain.yaml` is a semantic CGS model, not a deterministic OpenAPI dump.

Default loop:

```text
read spec/docs -> design graph -> author domain.yaml -> author mappings.yaml -> validate -> e2e test (Hermit, then live/sandbox) -> eval coverage -> iterate
```

Do not add scripts or generator crates that mechanically emit canonical `domain.yaml` or `mappings.yaml` from a spec.

## Validation Commands

Use these commands as appropriate:

```bash
cargo run -p plasm-cli --bin plasm -- schema validate apis/<api>
cargo run -p plasm-cli --bin plasm -- validate --schema apis/<api> --spec path/to/openapi.json
cargo run -p plasm-repl -- --schema apis/<api> --backend http://localhost:1080 --help
cargo run -p plasm-eval -- coverage --schema apis/<api> --cases apis/<api>/eval/cases.yaml
```

Use Hermit for mock-backed transport checks when an OpenAPI spec is available, then live or vendor sandbox testing per the e2e-test skill:

```bash
hermit --specs path/to/openapi.json --port 9090 --use-examples
cargo run -p plasm-repl -- --schema apis/<api> --backend http://localhost:9090
# In-session: expressions from DOMAIN; optional :output table
```

## Core Boundaries

Prefer catalog edits over core runtime changes. If an API cannot be represented with current CGS / CML / runtime semantics, stop and describe the missing expressiveness before modifying core crates.

Keep secrets out of schema files. Catalog auth reads from environment variables or supported runtime secret providers.
