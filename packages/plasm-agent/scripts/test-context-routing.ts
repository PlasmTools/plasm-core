#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { routingPacketSchema, routingRecoveryMarkdown } from "../src/engine/routing.js";

// The Rust discovery_service test derives this same fixture's coverage, matching,
// and recovery. Both consumers validate the identical serialized boundary.
const fixture = JSON.parse(readFileSync(new URL("../../../fixtures/discovery/partial-routing.json", import.meta.url), "utf8"));
const packet = routingPacketSchema.parse(fixture);
assert.equal(packet.routing.matching.complete, false);
assert.ok(packet.teaching);
assert.equal(packet.routing.closure?.business[0]?.capability, "record_read");
const markdown = routingRecoveryMarkdown(packet.routing);
assert.ok(markdown?.includes("teaching available"));
assert.ok(markdown?.includes("Publish the selected records after inspection"));
assert.ok(markdown?.includes("matrix/record_read"));
assert.ok(markdown?.includes("rejected"));
assert.deepEqual(routingPacketSchema.parse(JSON.parse(JSON.stringify(packet))), packet);

for (const corrupt of [
  (p: typeof packet) => { p.routing.matching.complete = true; },
  (p: typeof packet) => { p.routing.coverage.obligations.pop(); },
  (p: typeof packet) => { p.routing.coverage.obligations[0]!.matched_capabilities = []; },
  (p: typeof packet) => { p.routing.recovery!.unmatched_slots = []; },
  (p: typeof packet) => { p.routing.recovery!.unmatched_slots[0]!.statement = "Publish all records"; },
  (p: typeof packet) => { p.routing.recovery!.unmatched_slots[0]!.candidates = []; },
  (p: typeof packet) => { p.routing.authorization.capabilities.matrix = []; },
  (p: typeof packet) => { p.routing.closure!.business.push({ catalog: "unauthorized", capability: "read" }); },
]) {
  const changed = structuredClone(packet);
  corrupt(changed);
  assert.throws(() => routingPacketSchema.parse(changed));
}

const missing = structuredClone(packet);
missing.routing.retrieval.candidates = [];
missing.routing.matching.matches = [];
missing.routing.matching.unmatched_slot_ids = ["s0", "s1"];
missing.routing.matching.additional_capability_ids = [];
missing.routing.recovery!.unmatched_slots[0]!.candidates = [];
missing.routing.closure = null;
missing.teaching = null;
// Prior positive coverage survives a turn with no candidates.
routingPacketSchema.parse(missing);
assert.ok(routingRecoveryMarkdown(missing.routing)?.includes("No authorized candidates"));
console.log("context-routing: partial teaching, persistent coverage, authorization and Rust wire fixture passed");
