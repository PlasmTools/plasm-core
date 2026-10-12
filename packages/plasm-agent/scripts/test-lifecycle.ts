import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { MockLanguageModelV3 } from "ai/test";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";
import { PlasmAgent } from "../src/runtime/plasm-agent.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { sessionCases } from "./test-session-contract.js";
import { LocalArchiveStore } from "../src/archive/index.js";

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
const root = await mkdtemp(path.join(tmpdir(), "plasm-drain-"));
const unused = async (): Promise<never> => { throw new Error("unexpected engine call"); };
try {
  const entered = deferred<void>();
  const release = deferred<void>();
  const archived = deferred<void>();
  const releaseArchive = deferred<void>();
  const events: string[] = [];
  const engine: PlasmEngine = {
    loadCatalog: unused, activateDiscovery: unused, routeIntent: unused,
    synthesizeTeaching: unused, dryRun: unused, run: unused, introspectCatalog: unused,
    close: async () => { events.push("closed"); },
    runPlan: async () => {
      entered.resolve();
      await release.promise;
      return { ok: true, message: "completed", artifactsJson: JSON.stringify([
        { run_id: `pr${"a".repeat(64)}`, snapshot: { entities: [{ id: 1 }] } },
      ]) };
    },
  };
  let observed = "";
  const inputRuntime = new AgentRuntime({ agentRoot: root, archive: null, hostTransport: null, engine: {
    ...engine, close: async () => {},
    routeIntent: async (provenance) => {
      observed = provenance.nodes.at(-1)!.intent;
      throw new Error("snapshot observed");
    },
  } });
  const input = { intent: "original" };
  const snapshot = inputRuntime.plasmContext(input);
  input.intent = "mutated";
  await assert.rejects(snapshot, /snapshot observed/);
  assert.equal(observed, "original", "public context admission must capture inputs before returning");
  await inputRuntime.close();
  const archive = new LocalArchiveStore(path.join(root, "archive"));
  const runtime = new AgentRuntime({ agentRoot: root, engine, hostTransport: null, archive });
  const session = [...sessionCases(1)][0]!;
  Reflect.set(runtime, "workflowSession", session);
  // Hold the host persistence tail, not only the engine promise.
  const write = archive.writeRunSnapshot.bind(archive);
  archive.writeRunSnapshot = async (snapshot) => {
    archived.resolve();
    await releaseArchive.promise;
    await write(snapshot);
    events.push("persisted");
  };
  const run = runtime.plasmRun({ logicalSessionRef: session.logicalSessionRef, runRef: "pc0" });
  await entered.promise;
  const closing = runtime.close();
  assert.equal(runtime.close(), closing);
  await assert.rejects(runtime.plasmRun({ logicalSessionRef: session.logicalSessionRef, runRef: "pc0" }), /closing or closed/);
  assert.deepEqual(events, []);
  release.resolve();
  await archived.promise;
  assert.deepEqual(events, [], "close must wait for host persistence");
  releaseArchive.resolve();
  assert.match(await run, /completed/);
  await closing;
  assert.deepEqual(events, ["persisted", "closed"]);

  const started = deferred<void>();
  const finish = deferred<void>();
  const model = new MockLanguageModelV3({ doStream: async () => {
    started.resolve();
    await finish.promise;
    return { stream: new ReadableStream({ start(controller) {
      controller.enqueue({ type: "finish", finishReason: { unified: "stop", raw: undefined }, usage: {
        inputTokens: { total: 0, noCache: 0, cacheRead: 0, cacheWrite: 0 },
        outputTokens: { total: 0, text: 0, reasoning: 0 },
      } });
      controller.close();
    } }) };
  } });
  let closed = 0;
  const agent = new PlasmAgent({ agentRoot: root, model, engine: { ...engine, close: async () => { closed++; } },
    hostTransport: null, archive: null, telemetry: false });
  agent.runtime.hasOpenWorkflow = () => true;
  const turn = agent.generate("drain", { maxSteps: 1 });
  await started.promise;
  const shutdown = agent.close();
  await assert.rejects(agent.generate("too late"), /closing or closed/);
  assert.equal(closed, 0);
  finish.resolve();
  await turn;
  await shutdown;
  await agent.close();
  assert.equal(closed, 1);

  const failing = new AgentRuntime({ agentRoot: root, engine: { ...engine, runPlan: async () => { throw new Error("provider failed"); } }, archive: null, hostTransport: null });
  Reflect.set(failing, "workflowSession", session);
  const failure = failing.plasmRun({ logicalSessionRef: session.logicalSessionRef, runRef: "pc0" });
  const drained = failing.close();
  await assert.rejects(failure, /provider failed/);
  await drained;
  const planning = deferred<void>();
  const releasePlan = deferred<void>();
  const fused = new AgentRuntime({ agentRoot: root, tenantScope: session.tenantScope, archive: null, hostTransport: null, engine: {
    ...engine,
    dryRun: async () => {
      planning.resolve();
      await releasePlan.promise;
      return { writeCount: 0, planCommitRef: "pc1", summary: "read", fusedCleanRead: true };
    },
  } });
  Reflect.set(fused, "workflowSession", session);
  const acceptedPlan = fused.plasm({ logicalSessionRef: session.logicalSessionRef, program: "fixture program" });
  await planning.promise;
  const fusedClose = fused.close();
  releasePlan.resolve();
  assert.match(await acceptedPlan, /completed/, "accepted clean-read plan may finish its internal live run after admission closes");
  await fusedClose;
  console.log("PASS: turn and runtime admission, in-flight execution and persistence drain, idempotent close and preserved failures");
} finally { await rm(root, { recursive: true, force: true }); }
