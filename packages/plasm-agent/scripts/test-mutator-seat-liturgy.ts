#!/usr/bin/env node
import assert from "node:assert/strict";
import { buildDefaultSystemLiturgy, buildPlasmToolDescription } from "../src/prompts/index.js";
const card = buildPlasmToolDescription();
const liturgy = buildDefaultSystemLiturgy();
for (const text of [card, liturgy]) {
  for (const required of ["Program", "build(self)", "explicit return", "incremental domain declarations", "same logical session", "completed writes"])
    assert.ok(text.includes(required), `Missing Python protocol instruction: ${required}`);
  for (const retired of ["rows =>", "<<TAG", "e#(<id>)", "| where"])
    assert.ok(!text.includes(retired), `Retired syntax leaked: ${retired}`);
}
assert.equal(card, liturgy.slice(liturgy.indexOf("**Plasm** —")));
console.log("PASS: Python source contract and shared tool-description parity");
