import assert from "node:assert/strict";
import { readFile, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { AgentRuntime, AgentRuntimeHost } from "../src/runtime/agent-runtime.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { routingPacketSchema } from "../src/engine/routing.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { reviewPacketSchema } from "../src/runtime/workflow-learning.js";

const nativeReviewPacket = JSON.parse(await readFile(new URL("../../../fixtures/workflow-review-packet.json", import.meta.url), "utf8"));
assert.equal(reviewPacketSchema.parse(nativeReviewPacket).version, 4);

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
const engine: PlasmEngine = { takeCompletedTeaching: async () => "",
  loadCatalog: unused, activateDiscovery: unused, synthesizeTeaching: unused,
  dryRun: unused, runPlan: unused, run: unused, introspectCatalog: unused,
  close: async () => { events.push("closed"); },
  routeIntent: async provenance => {
    routed.resolve(); await release.promise;
    return { ...fixture, routing: { ...fixture.routing, intent_provenance: provenance } };
  },
  learning: {
    configure: async config => { events.push(config.mode); },
    configureTraining: async () => {},
    sealTrainingEvaluation: unused,
    trainingStatus: unused,
    trainingCommand: unused,
    trainingCandidates: unused,
    drainSession: async () => { events.push("drained"); },
    releaseSession: async () => {},
    completedExecutions: async id => { assert.equal(id, fixture.routing.pin_id); return { intent: "inspect", executions: [{ execution_id: `we${"a".repeat(64)}` }] }; },
    readProgram: async (id, run) => { assert.equal(id, fixture.routing.pin_id); return { version:4, execution_id:run, program:{state:"unavailable",reason:"missing_compiler_evidence"} }; },
    review: async (id, subtasks, revision, key) => {
      assert.equal(id, fixture.routing.pin_id); assert.deepEqual(subtasks[0]?.execution_ids, [`we${"a".repeat(64)}`]);
      assert.equal(revision, 3); assert.equal(key, "00000000-0000-4000-8000-000000000002");
      submitting.resolve(); await completeSubmission.promise; events.push("sealed"); return { review_ref: "review-one" };
    },
    fetchExample: unused,
  },
};
try {
  const runtime = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null, engine,
    workflowLearning: { mode: "collect", scope: { tenant: "t", project: "p" }, databaseUrl: "postgresql://fixture" },
    training: { subject: "test", credential: "host", idempotency_key: "00000000-0000-4000-8000-000000000001", grant: { grantee: "test", credential: "host", target: { scope: "project" }, actions: ["capture", "review"], expires_at: "2100-01-01T00:00:00Z", publication_policy: { policy: "human_reviewed" } }, requestForNewSessions: false },
  });
  assert.equal(createPlasmTools(runtime).plasm_example, undefined);
  assert.equal(createPlasmTools(runtime).plasm_training, undefined);
  assert.equal(createPlasmTools(runtime).plasm_training_candidates, undefined);
  assert.equal(createPlasmTools(runtime).plasm_review, undefined);
  const online = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null, engine: { ...engine, close: async () => {} },
    workflowLearning: { mode: "collect", scope: { tenant: "t", project: "p" }, databaseUrl: "postgresql://fixture", serveExamples: true },
  });
  assert.ok(createPlasmTools(online).plasm_example);
  await online.close();
  const context = runtime.plasmContext({ intent: "inspect" });
  await routed.promise;
  const reviewing = runtime.beginWorkflowReview();
  assert.equal(runtime.beginWorkflowReview(), reviewing);
  await assert.rejects(runtime.plasmContext({ intent: "new effects" }), /closing or closed/);
  assert.deepEqual(events, ["collect"]);
  release.resolve(); await context;
  const review = await reviewing;
  const index = await review.completedExecutions();
  const ref = index.sessions[0]!.logical_session_ref;
  assert.deepEqual(events, ["collect", "drained"]);
  assert.equal((await review.readProgram(ref, `we${"a".repeat(64)}`)).execution_id, `we${"a".repeat(64)}`);
  await assert.rejects(review.readProgram("l_AAAAAAAAAAAAAAAAAAAAAA", `we${"a".repeat(64)}`), /does not belong/);
  const seal = review.submitReview({ logical_session_ref: ref, expected_revision: 3, idempotency_key: "00000000-0000-4000-8000-000000000002", subtasks: [{ intent: "inspect", observed_outcome: "observed", execution_ids: [`we${"a".repeat(64)}`] }] });
  await submitting.promise;
  const closing = runtime.close();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(events.includes("closed"), false);
  completeSubmission.resolve(); await seal; await closing;
  assert.deepEqual(events, ["collect", "drained", "sealed", "closed"]);
  await assert.rejects(review.completedExecutions(), /closing or closed/);
  const finishedEvents: string[] = [];
  const finishedRuntime = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null,
    workflowLearning: { mode: "collect", scope: { tenant: "t", project: "p" }, databaseUrl: "postgresql://fixture" },
    training: { subject: "fixture", credential: "fixture", requestForNewSessions: false },
    engine: { ...engine, close: async () => {}, learning: { ...engine.learning!, configure: async () => {}, configureTraining: async () => {}, drainSession: async () => { finishedEvents.push("drained"); } } },
  });
  await finishedRuntime.plasmContext({ intent: "inspect" });
  await Promise.all([finishedRuntime.finishExecution(), finishedRuntime.finishExecution()]);
  assert.deepEqual(finishedEvents, ["drained"]);
  await assert.rejects(finishedRuntime.plasmContext({ intent: "later" }), /closing or closed/);
  await finishedRuntime.beginWorkflowReview();
  assert.deepEqual(finishedEvents, ["drained"]);
  await finishedRuntime.close();
  const baseline = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null, engine: { ...engine, learning: undefined } });
  await assert.rejects(baseline.beginWorkflowReview(), /requires explicit training delegation/);
  await baseline.close();
  const exampleRuntime = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null,
    workflowLearning: { mode: "examples", scope: { tenant: "t", project: "p" }, databaseUrl: "postgresql://fixture" },
    engine: { ...engine, learning: { ...engine.learning!, fetchExample: async () => ({
      example_id: "10000000-0000-4000-8000-000000000001", source: "class Example(Program): pass",
      teaching_delta: "# New checked e2 declaration", domain_revision: 2, delta_refs: ["matrix:Extra"],
      outputs: [],
    }) } },
  });
  assert.ok(createPlasmTools(exampleRuntime).plasm_example);
  await exampleRuntime.plasmContext({ intent: "inspect" });
  const taughtExample = await exampleRuntime.plasmExample({ logicalSessionRef: ref, revisionRef: "10000000-0000-4000-8000-000000000001" });
  assert.match(taughtExample, /class Example\(Program\)/);
  assert.match(taughtExample, /Required declarations/);
  assert.doesNotMatch(taughtExample, /domain_revision|delta_refs|catalog_fingerprints|row_type/);
  assert.match(exampleRuntime.workflowTeachingPrompt()!, /New checked e2 declaration/);
  await exampleRuntime.close();
  const unavailableRuntime = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null,
    engine: { ...engine, close: async () => {}, routeIntent: async provenance => ({
      ...fixture, routing: { ...fixture.routing, intent_provenance: provenance }, workflow_recall: "timed_out" as const,
    }) },
  });
  assert.match(await unavailableRuntime.plasmContext({ intent: "inspect" }), /Optional teaching examples are unavailable/);
  await unavailableRuntime.close();
  const pendingRuntime = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null,
    engine: { ...engine, close: async () => {}, routeIntent: async provenance => ({
      ...fixture, routing: { ...fixture.routing, intent_provenance: provenance }, workflow_recall: "pending" as const,
    }) },
  });
  assert.match(await pendingRuntime.plasmContext({ intent: "inspect" }), /will appear on a later session response/);
  await pendingRuntime.close();
  let configured = 0;
  let hostClosed = 0;
  const host = new AgentRuntimeHost({ ...engine,
    close: async () => { hostClosed++; },
    learning: { ...engine.learning!, configure: async () => { configured++; } },
  });
  const one = new AgentRuntime({ agentRoot: path.join(root, "one"), archive: null, hostTransport: null, host });
  const two = new AgentRuntime({ agentRoot: path.join(root, "two"), archive: null, hostTransport: null, host });
  await Promise.all([one.plasmContext({ intent: "one" }), two.plasmContext({ intent: "two" })]);
  assert.equal(configured, 1);
  assert.throws(() => host.close(), /Close all agent runtimes/);
  await one.close();
  assert.equal(hostClosed, 0);
  await two.close();
  await Promise.all([host.close(), host.close()]);
  assert.equal(hostClosed, 1);
  console.log("workflow review: admission closes, native evidence stays scoped, close drains seals");
} finally { await rm(root, { recursive: true, force: true }); }
