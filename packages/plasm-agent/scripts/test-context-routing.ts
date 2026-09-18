#!/usr/bin/env node
import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { routingPacketSchema, type RoutingPacket } from "../src/engine/routing.js";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";

const pin = "11111111-1111-4111-8111-111111111111";

const RECOVERY_GUIDANCE =
  "Partial capability coverage does not satisfy the full intent. Unresolved clauses above remain open. Searching rows searches data within taught capabilities; needing another kind of operation or another input source requires discovery extension — rephrase the unresolved need and call plasm_context again (session_mode extend with the same logical_session_ref when a logical session already exists; session_mode new only when none exists yet) so discovery can search again across the available integration descriptions listed below. Discovery exposes ways to investigate alternatives; it does not choose the intended record or authorize a substitute operation. Do not treat partial teaching as complete coverage.";

/** Recovery payload delivered by the host when selection is insufficient. */
function recoveryPayload(requirement: string, reason: string, supportingIds: string[] = []) {
  return {
    unresolved: [{ requirement, reason }],
    requirement_coverage: [
      {
        requirement,
        assessment: { useful_capabilities: supportingIds, missing: reason },
      },
    ],
    available_catalogs: [
      { entry_id: "matrix", description: "Abstract records for relational work" },
      { entry_id: "provider", description: "Value acquisition integration" },
    ],
    guidance: RECOVERY_GUIDANCE,
  };
}

function covered(requirement: string, ids: string[]) {
  return {
    requirement,
    assessment: { supported_by: ids },
  };
}

function unresolved(requirement: string, reason: string) {
  return {
    requirement,
    assessment: { useful_capabilities: [] as string[], missing: reason },
  };
}

function ready(capability = "read"): RoutingPacket {
  const ref = { catalog: "matrix", capability };
  return routingPacketSchema.parse({
    routing: {
      intent: "original workflow need",
      pin_id: pin,
      retrieval: { generation: "generation-one", candidates: [{ id: capability, reference: ref, document: { entity: "Record" } }] },
      selection: {
        status: "ready",
        additional_capability_ids: [capability],
        requirement_coverage: [covered("original workflow need", [capability])],
      },
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
  for (const entry of [
    { requirement: "read", supporting_capability_ids: ["read"], unresolved_reason: "" },
    { requirement: "read", assessment: { supported_by: ["read"], useful_capabilities: ["read"], missing: "input" } },
    { requirement: "read", assessment: { supported_by: [] } },
    { requirement: "read", assessment: { useful_capabilities: [], missing: " " } },
    { requirement: "read", assessment: { useful_capabilities: ["read", "read"], missing: "input" } },
  ]) {
    const packet = ready();
    assert.equal(routingPacketSchema.safeParse({
      ...packet,
      routing: { ...packet.routing, selection: { ...packet.routing.selection, requirement_coverage: [entry] } },
    }).success, false, "invalid assessment must not decode");
  }
  const contradictory = ready();
  assert.equal(routingPacketSchema.safeParse({
    ...contradictory,
    routing: { ...contradictory.routing, selection: {
      ...contradictory.routing.selection,
      requirement_coverage: [{ requirement: "read", assessment: { useful_capabilities: ["read"], missing: "input" } }],
    } },
  }).success, false, "a useful consumer does not make an unresolved requirement ready");
  const runtime = new AgentRuntime({ agentRoot: root, engine, archiveEnabled: false, hostTransport: null });
  const missing = ready();
  missing.routing.selection = {
    status: "insufficient",
    additional_capability_ids: [],
    requirement_coverage: [unresolved("make music", "Unavailable")],
  };
  missing.routing.closure = null;
  missing.teaching = null;
  missing.routing.recovery = recoveryPayload("make music", "Unavailable");
  replies.push(missing);
  const rejected = await runtime.plasmContext({ intent: "make music" });
  assert.match(rejected, /Unresolved/);
  assert.match(rejected, /make music/);
  assert.match(rejected, /Requirement coverage/);
  assert.match(rejected, /Abstract records for relational work/);
  assert.match(rejected, /Value acquisition integration/);
  assert.match(rejected, /new only when none exists yet/);
  assert.match(rejected, /Searching rows searches data/);
  assert.match(rejected, /does not choose the intended record/);
  assert.doesNotMatch(rejected, /Unsupported/);
  // Rejected insufficiency must not mint a session wire ref (guidance may still name the field).
  assert.doesNotMatch(rejected, /\*\*logical_session_ref:\*\*/);
  const partial = ready();
  partial.routing.selection.status = "insufficient";
  partial.routing.selection.requirement_coverage = [
    { requirement: "make music", assessment: { useful_capabilities: ["read"], missing: "Not in presented capabilities" } },
  ];
  partial.routing.recovery = recoveryPayload("make music", "Not in presented capabilities", ["read"]);
  replies.push(partial);
  partial.routing.intent_analysis = "Workflow interpretation: preserve the original selection scope";
  const opened = await runtime.plasmContext({ intent: "work on records", userRequests: ["Only records owned by colleagues"] });
  assert.match(opened, /preserve the original selection scope/);
  assert.match(opened, /selected read/);
  assert.match(opened, /Unresolved/);
  assert.match(opened, /`make music`/);
  assert.match(opened, /Requirement coverage/);
  assert.match(opened, /useful: read/);
  assert.match(opened, /Abstract records for relational work/);
  assert.match(opened, /extend with the same logical_session_ref/);
  assert.doesNotMatch(opened, /Unsupported/);
  assert.deepEqual(calls[1], ["work on records", undefined, ["Only records owned by colleagues"]]);
  const ref = opened.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
  assert.ok(ref);
  for (let n = 0; n < 2; n++) {
    const partialExtension = ready("write");
    partialExtension.routing.selection.status = "insufficient";
    partialExtension.routing.selection.requirement_coverage = [
      { requirement: "make music", assessment: { useful_capabilities: ["write"], missing: "Not presented" } },
    ];
    partialExtension.routing.recovery = recoveryPayload("make music", "Not presented", ["write"]);
    replies.push(partialExtension);
    const extended = await runtime.plasmContext({ intent: "write records", sessionMode: "extend", logicalSessionRef: ref });
    assert.match(extended, /selected write/);
    assert.match(extended, /Unresolved/);
    assert.match(extended, /Available integrations/);
    assert.match(extended, /`matrix`/);
    assert.doesNotMatch(extended, /Unsupported/);
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
  console.log("context-routing: insufficiency, recovery delivery, repeated extension, generation pin, prerequisites and execution identity passed");
} finally {
  await rm(root, { recursive: true, force: true });
}
