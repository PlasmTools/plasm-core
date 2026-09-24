#!/usr/bin/env node
import assert from "node:assert/strict";
import { PlasmEngine } from "@plasm_lang/engine";
import { workflowIntentSchema } from "../src/runtime/session-contract.js";
import { sessionCases } from "./test-session-contract.js";

// Rust decodes provenance before looking for a discovery generation. No network
// or model is involved; the activation error witnesses successful native decode.
const engine = new PlasmEngine();
for (const state of sessionCases(128)) {
  await assert.rejects(
    engine.routeIntent(JSON.stringify(state.intentProvenance)),
    /activateDiscovery/,
  );
}
for (const nodes of [
  [],
  [{ parent: 0, intent: "root" }],
  [{ intent: "root" }],
  [{ parent: null, intent: "root" }, { parent: null, intent: "child" }],
  [{ parent: null, intent: "root\0" }],
  [{ parent: null, intent: "root\ud800" }],
]) {
  await assert.rejects(engine.routeIntent(JSON.stringify({ nodes })),
    (error: unknown) => error instanceof Error && !error.message.includes("activateDiscovery"));
}
console.log("intent-native: 128 generated TypeScript -> JSON -> Rust chains accepted; malformed ancestry and Unicode rejected");

// Unicode White_Space must mean the same thing in both codecs; ECMAScript
// trim differs for NEXT LINE and ZERO WIDTH NO-BREAK SPACE.
for (const [intent, accepted] of [["\u0085", false], ["\ufeff", true], [" \u0085 ", false], ["  intent  ", true]] as const) {
  assert.equal(workflowIntentSchema.safeParse(intent).success, accepted);
  await assert.rejects(engine.routeIntent(JSON.stringify({ nodes: [{ parent: null, intent }] })),
    (error: unknown) => error instanceof Error && error.message.includes("activateDiscovery") === accepted);
}
