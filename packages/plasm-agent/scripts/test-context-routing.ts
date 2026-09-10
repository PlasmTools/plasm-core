#!/usr/bin/env node
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { routingPacketSchema, type RoutingPacket } from "../src/engine/routing.js";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";

const pin = "11111111-1111-4111-8111-111111111111";
function ready(capability = "read"): RoutingPacket {
  const ref = { catalog: "matrix", capability };
  return routingPacketSchema.parse({
    routing: {
      intent: "original workflow need",      pin_id: pin,
      retrieval: { generation: "generation-one", candidates: [{ id: capability, reference: ref, document: { entity: "Record" } }] },
      selection: { status: "ready", additional_capability_ids: [capability], unsupported: [] },
      closure: { business: [ref], prerequisites: [{ catalog: "provider", capability: "acquire" }], acquisitions: [{
        id: "acquisition", provider_catalog: "provider", provider: "value-provider", capability: { catalog: "provider", capability: "acquire" }, arguments: {},
      }], edges: [] },
    },
    teaching: { tsv: `e1\tRecord\nselected ${capability}`, delta_refs: ["matrix:Record"] },
  });
}
const replies: RoutingPacket[] = [];
const calls: Parameters<PlasmEngine["routeIntent"]>[] = [];
const drySessions: Array<string | undefined> = [];
const engine: PlasmEngine = {
  async loadCatalog() {},
  async activateDiscovery() { return "generation-one"; },
  async routeIntent(...args) {
    calls.push(args);
    const packet = replies.shift();
    assert.ok(packet, "unexpected routing call");
    return packet;
  },
  async synthesizeTeaching() { throw new Error("routing must use native canonical teaching"); },
  async dryRun(_program, session) {
    drySessions.push(session);
    return { planCommitRef: "pc0", summary: "review required", fusedCleanRead: false };
  },
  async runPlan() { return { ok: true, message: "reviewed" }; },
  async introspectCatalog() { throw new Error("unused"); },
  async run() { throw new Error("unused"); },
};
const root = await mkdtemp(path.join(tmpdir(), "plasm-context-routing-"));
try {
  const runtime = new AgentRuntime({ agentRoot: root, engine, archiveEnabled: false, hostTransport: null });
  const missing = ready();
  missing.routing.selection = { status: "insufficient", additional_capability_ids: [], unsupported: [{ intent_quote: "make music", reason: "Unavailable" }] };
  missing.routing.closure = null;
  missing.teaching = null;
  replies.push(missing);
  const rejected = await runtime.plasmContext({ intent: "make music" });
  assert.match(rejected, /Unsupported/);
  assert.doesNotMatch(rejected, /logical_session_ref/);
  const partial = ready();
  partial.routing.selection.status = "insufficient";
  partial.routing.selection.unsupported = [{ intent_quote: "make music", reason: "Not in presented capabilities" }];
  replies.push(partial);
  const opened = await runtime.plasmContext({ intent: "work on records" });
  assert.match(opened, /selected read/);
  assert.match(opened, /Unsupported/);
  assert.deepEqual(calls[1], ["work on records", undefined]);
  const ref = opened.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
  assert.ok(ref);
  for (let n = 0; n < 2; n++) {
    const partialExtension = ready("write");
    partialExtension.routing.selection.status = "insufficient";
    partialExtension.routing.selection.unsupported = [{ intent_quote: "make music", reason: "Not presented" }];
    replies.push(partialExtension);
    const extended = await runtime.plasmContext({ intent: "write records", sessionMode: "extend", logicalSessionRef: ref });
    assert.match(extended, /selected write/);
    assert.match(extended, /Unsupported/);
    assert.equal(calls.at(-1)?.[1], pin, "every extension routes against the native session");
  }
  const session = await runtime.sessionManager.getByLogicalRef(ref);
  assert.match(session?.intent ?? "", /original workflow need/);
  assert.equal(session?.registryGeneration, "generation-one");
  assert.equal(session?.routingClosures?.length, 3);
  assert.equal(session?.routingClosures?.[0]?.prerequisites[0]?.capability, "acquire");
  await runtime.plasm({ logicalSessionRef: ref, program: "e1" });
  assert.deepEqual(drySessions, [pin]);
  const changed = ready();
  changed.routing.retrieval.generation = "generation-two";
  replies.push(changed);
  await assert.rejects(runtime.plasmContext({ intent: "read", sessionMode: "extend", logicalSessionRef: ref }), /pinned registry/);
  assert.equal(session?.planCommits.length, 1, "extension preserves reviewed plan state");
  const restarted = new AgentRuntime({ agentRoot: root, engine, archiveEnabled: false, hostTransport: null });
  await assert.rejects(restarted.plasm({ logicalSessionRef: ref, program: "e1" }), /expired with its native engine/);
  console.log("context-routing: insufficiency, repeated extension, generation pin, prerequisites and execution identity passed");
} finally {
  await rm(root, { recursive: true, force: true });
}
