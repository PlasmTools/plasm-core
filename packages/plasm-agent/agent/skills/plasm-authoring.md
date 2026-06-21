# Plasm authoring

Progressive-disclosure skill for catalog-native agents.

- Author capability once in `catalogs/<api>/domain.yaml` + `mappings.yaml`.
- Run `npm run build:stubs` to emit typed clients under `.plasm/stubs/`.
- Channels and schedules call stubs directly (deterministic program API).
- The model loop uses `discover_capabilities` → `plasm_context` → `plasm` → `plasm_run`.
