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
    /use the seat shape the card left column prints/,
    `${name} must teach card-printed seat shape as the sole write law`,
  );
  assert.equal(
    text.includes("parens / token-identity braces / pathless"),
    false,
    `${name} must not list three seat families as a peer menu`,
  );
  assert.match(text, /e#\(<id>\)\.m#/, `${name} must teach identity-anchored e#(<id>).m#`);
  assert.match(
    text,
    /fill each from a bound value/,
    `${name} must teach hole-fill from a bound value / row field / string`,
  );
  assert.match(text, /of that hole's sort/, `${name} must require a value of that hole's sort`);
  assert.match(text, /Get identity/, `${name} must name <id> as the Get identity`);
  assert.match(
    text,
    /item = e#\(row\.<id_field>\)/,
    `${name} must teach bind-then-mutate from row.<id_field>`,
  );
  assert.match(text, /item = e#\(row\.id\)/, `${name} must teach bind-then-mutate from a row field`);
  assert.equal(
    text.includes('e#("…")'),
    false,
    `${name} must not teach e#("…") as generic mutator fill`,
  );
  assert.equal(
    text.includes("copy the card left-column seat exactly"),
    false,
    `${name} must not say copy-the-seat-exactly`,
  );
  assert.equal(
    text.includes("emit the receiver and holes"),
    false,
    `${name} must not license literal <id> via emit-the-holes`,
  );
  assert.equal(
    text.includes("holes that line prints"),
    false,
    `${name} must not tell the model to emit printed holes`,
  );
  assert.equal(text.includes("or pathless"), false, `${name} must not list pathless as a peer`);
  assert.equal(text.includes("pathless `e#.m#"), false, `${name} must not teach pathless e#.m# as a peer`);
  assert.equal(text.includes(" · pathless "), false, `${name} must not list pathless as an apply peer`);
  assert.equal(text.includes("Pathless login"), false, `${name} must not spotlight pathless login/create`);
  assert.equal(text.includes("item.update"), false, `${name} must not teach named .update`);
  assert.equal(text.includes("mLogin"), false, `${name} must not teach named mLogin`);
  assert.equal(text.includes("if pathless"), false, `${name} must not teach pathless-fail retry`);
  assert.equal(text.includes("retry with"), false, `${name} must not teach retry-with liturgy`);
  assert.equal(text.includes("**Recovery:**"), false, `${name} must not title a Recovery liturgy`);
  assert.equal(text.includes('e#("id")'), false, `${name} must not teach generic e#("id")`);
  assert.match(
    text,
    /filled Get-identity seat/,
    `${name} must seed iterate from the filled Get-identity / card seat`,
  );
}

assert.equal(card, liturgy.slice(liturgy.indexOf("**Plasm** —")));
console.log("PASS: mutator seating liturgy uses one card seat and fills <> holes");
