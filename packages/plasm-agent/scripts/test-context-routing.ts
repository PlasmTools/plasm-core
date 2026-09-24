#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { routingPacketSchema, routingRecoveryMarkdown } from "../src/engine/routing.js";

// Rust and TypeScript consume the same serialized relevance receipt.
const fixture = JSON.parse(readFileSync(new URL("../../../fixtures/discovery/partial-routing.json", import.meta.url), "utf8"));
const packet = routingPacketSchema.parse(fixture);
assert.ok(packet.teaching);
assert.equal(packet.routing.closure?.business[0]?.capability, "record_read");
assert.equal(routingRecoveryMarkdown(packet.routing),null);
assert.deepEqual(routingPacketSchema.parse(JSON.parse(JSON.stringify(packet))),packet);
for (const corrupt of [
 (p:typeof packet)=>{p.routing.matching.matches.push(p.routing.matching.matches[0]!);},
 (p:typeof packet)=>{p.routing.matching.matches[0]!.capability_id="unknown";},
 (p:typeof packet)=>{p.routing.matching.matches=[];},
 (p:typeof packet)=>{p.routing.matching.matches[0]!.probabilities={relevant:1};},
 (p:typeof packet)=>{p.routing.authorization.capabilities.matrix=[];},
 (p:typeof packet)=>{p.routing.closure!.business=[];},
]) { const changed=structuredClone(packet);corrupt(changed);assert.equal(routingPacketSchema.safeParse(changed).success,false); }
const missing=structuredClone(packet);
missing.routing.matching.matches[0]!.choice="unrelated";
missing.routing.matching.matches[0]!.probabilities={relevant:0,unrelated:1,uncertain:0};
missing.routing.closure=null;missing.teaching=null;
missing.routing.recovery={candidates:[{reference:missing.routing.retrieval.candidates[0]!.reference,choice:"unrelated",relevance_probability:0}],available_catalogs:[],guidance:"No relevant capabilities in this bounded packet."};
assert.ok(routingPacketSchema.safeParse(missing).success);
missing.routing.matching.matches[0]!.capability_id="unknown";
assert.equal(routingPacketSchema.safeParse(missing).success,false,"malformed diagnostics fail validation without throwing");
console.log("context-routing: relevance, authorization and wire invariants passed");

assert.equal(routingPacketSchema.safeParse({...packet, routing: {...packet.routing, input_source_projection: []}}).success, false, "retired graph evidence must be rejected");

assert.equal(routingPacketSchema.safeParse({...packet, routing: {...packet.routing, reranking: {}}}).success, false, "removed reranking contract must be rejected");

const bounded = structuredClone(packet);
const first = packet.routing.retrieval.candidates[0]!;
bounded.routing.authorization.capabilities = {};
bounded.routing.retrieval.candidates = Array.from({length:128}, (_, i) => ({
  ...first, id: `candidate${i}`, reference: {...first.reference, capability:`read${i}`},
}));
bounded.routing.matching.matches = bounded.routing.retrieval.candidates.map(c => ({
  capability_id:c.id, choice:"relevant" as const,
  probabilities:{relevant:1,unrelated:0,uncertain:0},confidence:1,
}));
bounded.routing.closure!.business = bounded.routing.retrieval.candidates.map(c => c.reference);
assert.deepEqual(routingPacketSchema.parse(JSON.parse(JSON.stringify(bounded))),bounded);
bounded.routing.matching.matches.pop();
assert.equal(routingPacketSchema.safeParse(bounded).success,false,"partial page coverage must fail decoding");
bounded.routing.retrieval.candidates.push({...first,id:"overflow"});
assert.equal(routingPacketSchema.safeParse(bounded).success,false,"union cannot exceed128");
