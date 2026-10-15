import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
const require = createRequire(import.meta.url);
const { PlasmEngine } = require("../index.js");
const engine = new PlasmEngine();
const declaration = await readFile(new URL("../index.d.ts", import.meta.url), "utf8");
const signature = declaration.split("\n").find((line) => line.includes("routeIntent("));
assert.ok(signature, "generated routeIntent declaration missing");
assert.doesNotMatch(signature, /routingRef|clarifyChoices/);
const provenance = JSON.stringify({ nodes: [{ parent: null, intent: "inspect" }] });
try {
  await assert.rejects(engine.routeIntent(provenance), /discovery must be activated before routing/);
  await assert.rejects(engine.routeIntent(provenance, "missing-session"), /discovery must be activated before routing/);
} finally {
  await engine.close();
}
console.log("PASS: native routing requires activated discovery; generated contract has intent provenance and session only");
