#!/usr/bin/env node
import assert from "node:assert/strict";
import {
  buildDefaultSystemLiturgy,
  buildPlasmToolDescription,
} from "../src/prompts/index.js";

const card = buildPlasmToolDescription();
const liturgy = buildDefaultSystemLiturgy();
for (const [name, text] of [
  ["plasm_tool", card],
  ["system_liturgy", liturgy],
] as const) {
  assert.match(
    text,
    /e#\(<id>\)\.m#\(args\)/,
    `${name}: known entity receiver`,
  );
  assert.match(text, /item\.m#\(args\)/, `${name}: bound singleton receiver`);
  assert.match(
    text,
    /rows => _\.m#\(args\)/,
    `${name}: row application receiver`,
  );
  assert.match(
    text,
    /row supplies identity and scope/,
    `${name}: row receiver semantics`,
  );
  assert.match(
    text,
    /Empty singleton receivers fail/,
    `${name}: singleton emptiness law`,
  );
  assert.match(
    text,
    /Use only holes printed by the card/,
    `${name}: catalog-defined inputs`,
  );
  assert.equal(
    text.includes("item.update"),
    false,
    `${name}: opaque operation symbols`,
  );
}
assert.equal(card, liturgy.slice(liturgy.indexOf("**Plasm** —")));
console.log("PASS: canonical receivers and shared language-card parity");
