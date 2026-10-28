#!/usr/bin/env node
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { MockLanguageModelV3 } from "ai/test";
import { PlasmAgent } from "../src/runtime/plasm-agent.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { decodeRoutingPacket, routingExplanationLines, routingPacketSchema, routingRecoveryMarkdown } from "../src/engine/routing.js";
import { AgentExecutionFailure, failureObservation } from "../src/runtime/execution-failure.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { failureObservationSchema } from "../src/runtime/execution-failure.js";

// Rust and TypeScript consume the same serialized relevance receipt.
const fixture = JSON.parse(readFileSync(new URL("../../../fixtures/discovery/partial-routing.json", import.meta.url), "utf8"));
const packet = routingPacketSchema.parse(fixture);
// An existing surface with no additions is valid and is not an unmatched initial search.
const noAdditions = structuredClone(packet);
noAdditions.routing.search.exposed = packet.routing.retrieval.candidates.map(c => c.reference);
noAdditions.routing.retrieval.candidates = [];
noAdditions.routing.matching.matches = [];
noAdditions.routing.search.considered = [];
noAdditions.routing.search.unexplored = [];
noAdditions.routing.search.deferred = [];
noAdditions.routing.search.requests = 0;
noAdditions.routing.search.input_bytes = 0;
noAdditions.routing.search.reported_input_tokens = 0;
noAdditions.routing.search.missing_usage = 0;
noAdditions.routing.closure = {business:[],input_sources:[],prerequisites:[],acquisitions:[],edges:[]};
noAdditions.routing.recovery = null;
assert.deepEqual(decodeRoutingPacket(JSON.stringify(noAdditions)), noAdditions);
// A blocked new acquisition path cannot defer an already available declaration.
const deferredExisting = structuredClone(noAdditions);
deferredExisting.routing.search.deferred = [{
  reference: deferredExisting.routing.search.exposed[0]!, reason: "input_bytes",
}];
assert.throws(() => decodeRoutingPacket(JSON.stringify(deferredExisting)), AgentExecutionFailure);
const invalidEmptyInitial = structuredClone(noAdditions);
invalidEmptyInitial.routing.search.exposed = [];
assert.throws(() => decodeRoutingPacket(JSON.stringify(invalidEmptyInitial)), AgentExecutionFailure);
const rejudged = structuredClone(packet);
rejudged.routing.search.exposed = [packet.routing.retrieval.candidates[0]!.reference];
assert.throws(() => decodeRoutingPacket(JSON.stringify(rejudged)), AgentExecutionFailure);
// Deferred work is unexplored, not an uncertain judgment or exposed declaration.
const budget = structuredClone(packet);
budget.routing.search.unexplored = [{catalog:"matrix",capability:"other"}];
budget.routing.search.deferred = [{reference:{catalog:"matrix",capability:"other"},reason:"input_bytes"}];
assert.deepEqual(decodeRoutingPacket(JSON.stringify(budget)), budget);
const retried = structuredClone(budget);
retried.routing.search.requests = 3;
retried.routing.search.input_bytes *= 3;
retried.routing.search.missing_usage = 2;
assert.deepEqual(decodeRoutingPacket(JSON.stringify(retried)), retried);
for (const corrupt of [
  (p: typeof retried) => { p.routing.search.requests = 4; },
  (p: typeof retried) => { p.routing.search.input_bytes = 48001; },
  (p: typeof retried) => { p.routing.search.input_bytes += 1; },
  (p: typeof retried) => { p.routing.search.missing_usage = 4; },
]) {
  const invalid = structuredClone(retried); corrupt(invalid);
  assert.throws(() => decodeRoutingPacket(JSON.stringify(invalid)), AgentExecutionFailure);
}
// A required acquisition remains exposed even if its separate contribution
// question was deferred. Considered support for a rejected root stays unexplored.
const support = {catalog: "matrix", capability: "acquire"};
const requiredDeferred = structuredClone(packet);
requiredDeferred.routing.search.considered.push(support);
requiredDeferred.routing.closure!.prerequisites.push(support);
requiredDeferred.routing.search.deferred.push({reference: support, reason: "input_bytes"});
assert.deepEqual(decodeRoutingPacket(JSON.stringify(requiredDeferred)), requiredDeferred);
const unretainedSupport = structuredClone(packet);
unretainedSupport.routing.search.considered.push(support);
unretainedSupport.routing.search.unexplored.push(support);
assert.deepEqual(decodeRoutingPacket(JSON.stringify(unretainedSupport)), unretainedSupport);
for (const corrupt of [
  (p:typeof budget) => {p.routing.search.requests = 4;},
  (p:typeof budget) => {p.routing.search.input_bytes = 16001;},
  (p:typeof budget) => {p.routing.search.missing_usage = 2;},
  (p:typeof budget) => {p.routing.search.considered = [];},
  (p:typeof budget) => {p.routing.search.unexplored.push(p.routing.search.considered[0]!);},
  (p:typeof budget) => {p.routing.closure!.business = [];},
  (p:typeof budget) => {p.routing.authorization.capabilities.matrix = [];},
]) {
  const invalid = structuredClone(budget); corrupt(invalid);
  assert.throws(() => decodeRoutingPacket(JSON.stringify(invalid)), AgentExecutionFailure);
}
assert.throws(() => decodeRoutingPacket(JSON.stringify({...budget, routing: {...budget.routing,
  matching: {...budget.routing.matching, budget_retained: [{capability_id:"matrix/record_read",
    reference:{catalog:"matrix",capability:"record_read"},capabilities:9,capability_limit:8,bytes:12000,byte_limit:16000}]},
}})), AgentExecutionFailure);
assert.ok(packet.teaching);
assert.equal(packet.routing.closure?.business[0]?.capability, "record_read");
assert.equal(routingRecoveryMarkdown(packet.routing),null);
assert.deepEqual(routingPacketSchema.parse(JSON.parse(JSON.stringify(packet))),packet);
// Both mixed and uncertain-only retained sets cross the actual native decoder.
for (const mixed of [false, true]) {
  const retained = structuredClone(packet);
  const candidate = structuredClone(retained.routing.retrieval.candidates[0]!);
  if (mixed) {
    candidate.id = "uncertain";
    candidate.reference.capability = "record_inspect";
    retained.routing.retrieval.candidates.push(candidate);
    retained.routing.search.considered.push(candidate.reference);
    retained.routing.closure!.business.push(candidate.reference);
    retained.routing.authorization.capabilities = {};
    retained.routing.matching.matches.push({...retained.routing.matching.matches[0]!, capability_id: candidate.id});
  }
  const decision = retained.routing.matching.matches.at(-1)!;
  decision.choice = "uncertain";
  assert.deepEqual(decodeRoutingPacket(JSON.stringify(retained)), retained);
  assert.deepEqual(routingExplanationLines(retained.routing.matching), []);
  const excluded = structuredClone(retained);
  excluded.routing.closure!.business.pop();
  assert.throws(() => decodeRoutingPacket(JSON.stringify(excluded)), AgentExecutionFailure);
  retained.routing.authorization.capabilities[candidate.reference.catalog] = [];
  assert.throws(() => decodeRoutingPacket(JSON.stringify(retained)), AgentExecutionFailure);
}
for (const raw of ["{", JSON.stringify({...packet, routing: {...packet.routing, closure:null}})]) {
  try { decodeRoutingPacket(raw); assert.fail("invalid packet accepted"); } catch (error) {
    assert.ok(error instanceof AgentExecutionFailure);
    const observation = failureObservation(error);
    assert.equal(observation.failure.code, "discovery_response_contract");
    assert.equal(observation.failure.effects_unresolved, false);
    assert.equal(observation.failure.recovery, "stop");
    assert.match(observation.summary, /Discovery response failed contract validation/);
    assert.doesNotMatch(observation.summary, /unclassified|Effects may be unresolved/);
  }
}
const failingTools = createPlasmTools({
  plasmContext: async () => { decodeRoutingPacket("{"); return "unreachable"; },
  plasm: async () => "unused", plasmRun: async () => "unused",
  recordExecutionFailure: async () => {},
});
const failedContext = failureObservationSchema.parse(await failingTools.plasm_context!.execute!(
  {intent:"Read records", session_mode:"new"}, {toolCallId:"contract", messages:[], context:{}},
));
assert.equal(failedContext.failure.code, "discovery_response_contract");
assert.match(failedContext.summary, /not valid JSON/);
assert.match(failedContext.summary, /End of recorded failure details/);
assert.doesNotMatch(failedContext.summary, /call plasm_failure/);
assert.equal(failedContext.failure.effects_unresolved, false);
for (const corrupt of [
 (p:typeof packet)=>{p.routing.matching.matches.push(p.routing.matching.matches[0]!);},
 (p:typeof packet)=>{p.routing.matching.matches[0]!.capability_id="unknown";},
 (p:typeof packet)=>{p.routing.matching.matches=[];},

 (p:typeof packet)=>{p.routing.authorization.capabilities.matrix=[];},
 (p:typeof packet)=>{p.routing.closure!.business=[];},
]) { const changed=structuredClone(packet);corrupt(changed);assert.equal(routingPacketSchema.safeParse(changed).success,false); }
const missing=structuredClone(packet);
missing.routing.matching.matches[0]!.choice="unrelated";
missing.routing.closure=null;missing.teaching=null;
missing.routing.recovery={candidates:[{reference:missing.routing.retrieval.candidates[0]!.reference,choice:"unrelated"}],available_catalogs:[],guidance:"No relevant capabilities in this bounded packet."};
assert.ok(routingPacketSchema.safeParse(missing).success);
missing.routing.matching.matches[0]!.capability_id="unknown";
assert.equal(routingPacketSchema.safeParse(missing).success,false,"malformed diagnostics fail validation without throwing");
console.log("context-routing: relevance, authorization and wire invariants passed");

assert.equal(routingPacketSchema.safeParse({...packet, routing: {...packet.routing, input_source_projection: []}}).success, false, "retired graph evidence must be rejected");

assert.equal(routingPacketSchema.safeParse({...packet, routing: {...packet.routing, reranking: {}}}).success, false, "removed reranking contract must be rejected");

const bounded = structuredClone(packet);
const first = packet.routing.retrieval.candidates[0]!;
bounded.routing.authorization.capabilities = {};
bounded.routing.retrieval.candidates = Array.from({length:8}, (_, i) => ({
  ...first, id: `candidate${i}`, reference: {...first.reference, capability:`read${i}`},
}));
bounded.routing.matching.matches = bounded.routing.retrieval.candidates.map(c => ({
  capability_id:c.id, choice:"relevant" as const,
}));
bounded.routing.closure!.business = bounded.routing.retrieval.candidates.map(c => c.reference);
bounded.routing.search.considered = bounded.routing.retrieval.candidates.map(c => c.reference);
assert.deepEqual(routingPacketSchema.parse(JSON.parse(JSON.stringify(bounded))),bounded);
bounded.routing.matching.matches.pop();
assert.equal(routingPacketSchema.safeParse(bounded).success,false,"partial page coverage must fail decoding");
bounded.routing.retrieval.candidates.push({...first,id:"overflow"});
assert.equal(routingPacketSchema.safeParse(bounded).success,false,"online search cannot exceed eight considered members");

for (const retired of [null, {choice: "established", request_hash: "x"}]) {
  assert.equal(routingPacketSchema.safeParse({...packet, routing: {...packet.routing, environment_support: retired}}).success, false, "retired support contract must be rejected");
}
// Optional input is generated by the Rust example, never a parallel TS mock.
if (process.env.PLASM_ROUTING_CONTRACT) {
  const cases = JSON.parse(readFileSync(process.env.PLASM_ROUTING_CONTRACT, "utf8"));
  assert.equal(cases.length, 5);
  for (const entry of cases) {
    const decoded = routingPacketSchema.parse(entry.packet);
    assert.deepEqual(decodeRoutingPacket(JSON.stringify(entry.packet)), decoded);
    await proveBootstrap(entry.packet);
  }
  console.log("PASS: actual Rust receipt serialization and contribution-only routing");
}

// Exercise decoding -> runtime context -> tool result -> first model request.
// No provider or business execution is involved in this abstract boundary proof.
async function proveBootstrap(wire: unknown): Promise<void> {
  const expected = decodeRoutingPacket(JSON.stringify(wire));
  const root = await mkdtemp(path.join(tmpdir(), "plasm-budget-routing-"));
  const unused = async (): Promise<never> => {throw new Error("unexpected execution");};
  let routes = 0;
  const engine: PlasmEngine = {
    close: async () => {}, loadCatalog: unused, activateDiscovery: unused,
    synthesizeTeaching: unused, dryRun: unused, runPlan: unused, run: unused, introspectCatalog: unused,
    routeIntent: async provenance => {
      routes++;
      return decodeRoutingPacket(JSON.stringify({...expected, routing: {...expected.routing, intent_provenance: provenance}}));
    },
  };
  const model = new MockLanguageModelV3({doStream: async request => {
    assert.ok(JSON.stringify(request.prompt).includes(expected.teaching!.prompt));
    return {stream: new ReadableStream({start(controller) {
      controller.enqueue({type: "text-start", id: "text"});
      controller.enqueue({type: "text-delta", id: "text", delta: "Observed declarations."});
      controller.enqueue({type: "text-end", id: "text"});
      controller.enqueue({type: "finish", finishReason: {unified: "stop", raw: undefined}, usage: {
        inputTokens: {total: 1, noCache: 1, cacheRead: 0, cacheWrite: 0},
        outputTokens: {total: 1, text: 1, reasoning: 0},
      }});
      controller.close();
    }})};
  }});
  try {
    const agent = new PlasmAgent({agentRoot: root, model, engine, hostTransport: null,
      archiveEnabled: false, telemetry: false, includeEvalTerminals: true});
    await agent.generate("Read selected records and publish them after inspection.");
    assert.equal(routes, 1);
    assert.equal(model.doStreamCalls.length, 1);
  } finally {await rm(root, {recursive: true, force: true});}
}
await proveBootstrap(budget);

assert.equal(routingPacketSchema.safeParse({...packet, routing: {...packet.routing, matching: {matches: [{...packet.routing.matching.matches[0], probabilities: {relevant:1}, confidence:1}]}}}).success, false, "invented confidence is not admitted");
