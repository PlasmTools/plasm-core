#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { buildDefaultSystemLiturgy, buildPlasmToolDescription } from "../src/prompts/index.js";
const card = buildPlasmToolDescription();
const liturgy = buildDefaultSystemLiturgy();
for (const asset of ["initialize_workflow.txt", "plasm_context_tool.txt", "plasm_run_tool_base.txt", "plasm_tool.txt", "python-plasm-dag.txt"]) {
  assert.equal(
    readFileSync(new URL(`../src/prompts/assets/${asset}`, import.meta.url), "utf8"),
    readFileSync(new URL(`../../../crates/plasm-core/src/prompt_render/assets/${asset}`, import.meta.url), "utf8"),
    `packaged ${asset} must match the canonical asset`,
  );
}
for (const text of [card]) {
  for (const required of ["Program", "build(self)", "explicit return", "incremental domain declarations", "same logical session", "completed writes"])
    assert.ok(text.includes(required), `Missing Python protocol instruction: ${required}`);
  for (const retired of ["rows =>", "<<TAG", "e#(<id>)", "| where"])
    assert.ok(!text.includes(retired), `Retired syntax leaked: ${retired}`);
}
assert.match(card, /Program\.build:/);
assert.match(card, /Cards: exact signatures;/);
assert.ok(!liturgy.includes("**Plasm** —"), "system instructions must not duplicate the tool protocol");
assert.ok(!liturgy.includes("Program.build:"), "system instructions must not duplicate the semantic reference");
assert.match(liturgy, /logical_session_ref/);
console.log("PASS: tool descriptions own the Python reference; system instructions own workflow");
