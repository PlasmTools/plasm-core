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
  (p: typeof packet) => { p.routing.coverage.current_slots.push("undeclared"); },
  (p: typeof packet) => { p.routing.coverage.current_slots.push("s0"); },
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

// A revised current need retains history without reactivating an unresolved goal.
const revised = structuredClone(packet);
revised.routing.coverage.current_slots = ["s0"];
revised.routing.matching.slots = revised.routing.matching.slots.filter((slot) => slot.id === "s0");
revised.routing.matching.matches = revised.routing.matching.matches.filter((match) => match.slot_id === "s0");
revised.routing.matching.complete = true;
revised.routing.matching.unmatched_slot_ids = [];
revised.routing.recovery = null;
const revisedWire = routingPacketSchema.parse(JSON.parse(JSON.stringify(revised)));
assert.deepEqual(revisedWire.routing.coverage.obligations, packet.routing.coverage.obligations);
assert.equal(routingRecoveryMarkdown(revisedWire.routing), null);

for (const source_identity of [
  { kind: "field" as const, field: "email" },
  { kind: "relation" as const, relation: "owners" },
]) {
  const candidate = { provider: {catalog: "matrix", capability: "directory_read"}, projection: {entity: "Directory", fields: {id: {kind: "direct" as const}}}, bindings: [],
    membership: [{source: {catalog: "matrix", capability: "record_read"}, source_identity, provider_identity: {kind: "field" as const, field: "id"}}] };
  const withEvidence = structuredClone(packet);
  withEvidence.routing.authorization.capabilities.matrix?.push("directory_read");
  withEvidence.routing.input_source_projection = [candidate];
  withEvidence.routing.input_source_matching = {selected: [], matches: [{provider: candidate.provider,
    choice: "not_required", probabilities: {required_source: 0, not_required: 1, uncertain: 0}, confidence: 1}]};
  const decoded = routingPacketSchema.parse(JSON.parse(JSON.stringify(withEvidence)));
  assert.deepEqual(decoded.routing.input_source_projection, [candidate]);
  withEvidence.routing.input_source_projection[0]!.membership[0]!.source.catalog = "unauthorized";
  assert.throws(() => routingPacketSchema.parse(withEvidence));
  withEvidence.routing.input_source_projection[0]!.membership = [];
  assert.throws(() => routingPacketSchema.parse(withEvidence), "empty evidence must be rejected");
}

// Every new native variant survives the strict TypeScript boundary; unauthorized
// hydration remains rejected rather than being stripped or accepted as unknown data.
for (const input of [
  {kind: "receiver" as const, entity: "Record"},
  {kind: "argument" as const, input: {lane: "arguments" as const, path: ["recipient"]}},
]) {
  const value = structuredClone(packet);
  value.routing.authorization.capabilities.matrix = ["record_read", "record_publish", "directory_read", "detail_read"];
  const provider = {catalog: "matrix", capability: "directory_read"};
  value.routing.input_source_projection = [{ provider,
    projection: {entity: "Directory", fields: {
      id: {kind: "direct"}, email: {kind: "hydrated", capability: "detail_read"}, secret: {kind: "unavailable"},
    }},
    bindings: [{provider, consumer: {catalog: "matrix", capability: "record_read"}, input, output_field: "id", collect: false}], membership: [],
  }];
  value.routing.input_source_matching = {selected: [], matches: [{provider, choice: "not_required",
    probabilities: {required_source: 0, not_required: 1, uncertain: 0}, confidence: 1}]};
  assert.deepEqual(routingPacketSchema.parse(JSON.parse(JSON.stringify(value))), value);
  value.routing.authorization.capabilities.matrix = value.routing.authorization.capabilities.matrix!.filter(c => c !== "detail_read");
  assert.throws(() => routingPacketSchema.parse(value), "unauthorized hydration must be rejected");
}
