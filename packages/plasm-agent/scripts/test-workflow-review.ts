import assert from "node:assert/strict";
import { readFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { routingPacketSchema } from "../src/engine/routing.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(done => { resolve = done; });
  return { promise, resolve };
}
const root = await mkdtemp(path.join(tmpdir(), "plasm-review-"));
const fixture = routingPacketSchema.parse(JSON.parse(await readFile(new URL("../../../fixtures/discovery/partial-routing.json", import.meta.url), "utf8")));
const unused = async (): Promise<never> => { throw new Error("unexpected effect"); };
const routed = deferred(); const release = deferred();
const submitting = deferred(); const completeSubmission = deferred();
const events: string[] = [];
const engine: PlasmEngine = {
  loadCatalog: unused, activateDiscovery: unused, synthesizeTeaching: unused,
  dryRun: unused, runPlan: unused, run: unused, introspectCatalog: unused,
  close: async () => { events.push("closed"); },
  routeIntent: async provenance => {
    routed.resolve(); await release.promise;
    return { ...fixture, routing: { ...fixture.routing, intent_provenance: provenance } };
  },
  learning: {
    configure: async config => { events.push(config.mode); },
    drain: async () => { events.push("drained"); },
    completedRuns: async id => { assert.equal(id, fixture.routing.pin_id); return { intent: "inspect", runs: [{ run_id: "run-one" }] }; },
    readEvidence: async (id, run) => { assert.equal(id, fixture.routing.pin_id); return { run_id: run }; },
    review: async (id, subtasks) => {
      assert.equal(id, fixture.routing.pin_id); assert.deepEqual(subtasks[0]?.run_ids, ["run-one"]);
      submitting.resolve(); await completeSubmission.promise; events.push("sealed"); return { review_ref: "review-one" };
    },
    fetchExample: unused,
  },
};
try {
  const runtime = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null, engine,
    workflowLearning: { mode: "collect", scope: { tenant: "t", project: "p" }, databaseUrl: "postgresql://fixture" },
  });
  assert.equal(createPlasmTools(runtime).plasm_example, undefined);
  const context = runtime.plasmContext({ intent: "inspect" });
  await routed.promise;
  const reviewing = runtime.beginWorkflowReview();
  assert.equal(runtime.beginWorkflowReview(), reviewing);
  await assert.rejects(runtime.plasmContext({ intent: "new effects" }), /closing or closed/);
  assert.deepEqual(events, ["collect"]);
  release.resolve(); await context;
  const review = await reviewing;
  const index = await review.completedRuns();
  const ref = index.sessions[0]!.logical_session_ref;
  assert.deepEqual(events, ["collect", "drained"]);
  assert.deepEqual(await review.readEvidence(ref, "run-one"), { run_id: "run-one" });
  await assert.rejects(review.readEvidence("l_AAAAAAAAAAAAAAAAAAAAAA", "run-one"), /does not belong/);
  const seal = review.submitReview({ logical_session_ref: ref, subtasks: [{ intent: "inspect", observed_outcome: "observed", run_ids: ["run-one"] }] });
  await submitting.promise;
  const closing = runtime.close();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(events.includes("closed"), false);
  completeSubmission.resolve(); await seal; await closing;
  assert.deepEqual(events, ["collect", "drained", "sealed", "closed"]);
  await assert.rejects(review.completedRuns(), /closing or closed/);
  const baseline = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null, engine: { ...engine, learning: undefined } });
  await assert.rejects(baseline.beginWorkflowReview(), /requires collect/);
  await baseline.close();
  const exampleRuntime = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null,
    workflowLearning: { mode: "examples", scope: { tenant: "t", project: "p" }, databaseUrl: "postgresql://fixture" },
    engine: { ...engine, learning: { ...engine.learning!, fetchExample: async () => ({
      example_id: "10000000-0000-4000-8000-000000000001", source: "class Example(Program): pass",
      teaching_delta: "# New checked e2 declaration", domain_revision: 2, delta_refs: ["matrix:Extra"],
      outputs: [], catalog_fingerprints: { matrix: "fixture" },
    }) } },
  });
  assert.ok(createPlasmTools(exampleRuntime).plasm_example);
  await exampleRuntime.plasmContext({ intent: "inspect" });
  await exampleRuntime.plasmExample({ logicalSessionRef: ref, revisionRef: "10000000-0000-4000-8000-000000000001" });
  assert.match(exampleRuntime.workflowTeachingPrompt()!, /New checked e2 declaration/);
  await exampleRuntime.close();
  console.log("workflow review: admission closes, native evidence stays scoped, close drains seals");
} finally { await rm(root, { recursive: true, force: true }); }
