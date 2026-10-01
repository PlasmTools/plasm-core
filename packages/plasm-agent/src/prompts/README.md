# Prompt assets

Canonical bytes live in `plasm-oss/crates/plasm-core/src/prompt_render/assets/`.

This package vendors copies under `assets/` for published npm. In a monorepo
checkout, `loadPromptAsset` prefers the crates path so MCP and PlasmAgent stay
byte-identical without a sync step.

`PlasmAgent.loadInstructions()` always prepends `buildDefaultSystemLiturgy()`
(initialize workflow + resource rites + `plasm_tool.txt`). With
`includeEvalTerminals`, `overlayWorkflowCompletion` replaces the named
`WORKFLOW_COMPLETION_SLOT` paragraph with `EVAL_TERMINAL_COMPLETION`
(`complete_task` / `submit_answer`) — same gate that registers those tools.
The slot is the product no-tool-call contract text, not a paragraph index.
Project `instructions.md` is an overlay only.

After editing crates assets, refresh vendored copies:

```bash
  plasm-oss/packages/plasm-agent/src/prompts/assets/
```
