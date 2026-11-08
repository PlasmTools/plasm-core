#!/usr/bin/env node
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { type IntentProvenance } from "../src/runtime/session-contract.js";
import { routingPacketSchema } from "../src/engine/routing.js";

export async function checkSessionExtension(observe: (intent: string) => Promise<void> = async () => { }, exercise?: (runtime: AgentRuntime, ref: string, engine: PlasmEngine) => Promise<void>): Promise<void> {
  const root = await mkdtemp(path.join(tmpdir(), "plasm-extension-"));
  const routed: IntentProvenance[] = [];
  let insufficient = false;
  let malformedTeaching = false;
  let guidanceOnly = false;
  const provenanceBySession = new Map<string, IntentProvenance>();
  const unused = async (): Promise<never> => { throw new Error("unexpected engine operation"); };
  const engine: PlasmEngine = { sealEffectWorkflows: async () => {}, resumeEffectRun: async () => { throw new Error("unexpected repair"); }, takeCompletedTeaching: async () => "",
  close: async () => {},
    loadCatalog: unused, synthesizeTeaching: unused, dryRun: unused,
    activateDiscovery: unused, runPlan: unused, run: unused, introspectCatalog: unused,
    async routeIntent(provenance, sessionId) {
      const intent = provenance.nodes.at(-1)!.intent;
      // PostgreSQL rejects a NUL in the text-search query parameter. Exercise the
      // real runtime + filesystem path before crossing this native boundary.
      assert.ok(!intent.includes("\0"), "discovery intent contains a storage-key NUL");
      await observe(intent);
      routed.push(provenance);
      const pin = sessionId ?? randomUUID();
      const matches=[{capability_id:"matrix:read",choice:insufficient?"unrelated":"relevant"}];
      const packet = routingPacketSchema.parse({
        routing: {
          intent_provenance: provenance, intent, pin_id: pin,
          authorization: { catalogs: ["matrix"], capabilities: {} },
          retrieval: {
            generation: "fixture", candidates: [{
              id: "matrix:read", reference: { catalog: "matrix", capability: "read" },
              document: { entity: "Record" },
            }]
          },
          matching: {matches},
          search: {policy: "additive-connected-frontier-current-hybrid-evidence-3-judgments-3-attempts-16000-byte-packet-v37",
            exposed: [],
            considered: [{catalog:"matrix",capability:"read"}], unexplored: [], deferred: [],
            requests: 1, input_bytes: 1000, reported_input_tokens: 200, missing_usage: 0},
          recovery: insufficient ? {candidates:[{reference:{catalog:"matrix",capability:"read"},choice:"unrelated"}],available_catalogs:[],guidance:"No relevant capability in this bounded packet."}:null,
          closure: insufficient ? (sessionId ? { business: [], input_sources: [], prerequisites: [], acquisitions: [], edges: [] } : null) : { business: [{ catalog: "matrix", capability: "read" }], input_sources: [], prerequisites: [], acquisitions: [], edges: [] },
        },
        teaching: insufficient ? (sessionId ? { prompt: "", declarations_changed: false, delta_refs: [] } : null) : { prompt: "e1\tRecord", declarations_changed: true, delta_refs: ["matrix:Record"] },
      });
      if (malformedTeaching) {
        packet.teaching = { prompt: "", declarations_changed: true, delta_refs: ["matrix:Record"] };
        return packet;
      }
      if (guidanceOnly) {
        packet.teaching = { prompt: "Bind credential from the existing source.", declarations_changed: false, delta_refs: ["matrix:Record"] };
      }
      const previous = provenanceBySession.get(pin);
      if (previous) {
        assert.deepEqual(provenance.nodes.slice(0, previous.nodes.length), previous.nodes,
          "intent provenance rewrites or omits pinned ancestry");
      }
      provenanceBySession.set(pin, structuredClone(provenance));
      return packet;
    },
  };
  try {
    const partialRuntime = new AgentRuntime({ agentRoot: root, engine, archive: null, hostTransport: null });
    const partiallyOpened = await partialRuntime.plasmContext({ intent: "Read selected records then publish them",  });
    assert.ok(partiallyOpened.includes("e1\tRecord"));
    assert.doesNotMatch(partiallyOpened, /support classifier|support.*undetermined|establish a plan/i);
    const partialRef = partiallyOpened.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(partialRef);
    const beforePartial = await partialRuntime.sessionManager.getByLogicalRef(partialRef);
    assert.ok(beforePartial);
    assert.ok(beforePartial.teachingPrompt.includes("Record"));
    insufficient = true;
    const unresolved = await partialRuntime.plasmContext({ intent: "Find a related record",  sessionMode: "extend", logicalSessionRef: partialRef });
    assert.ok(unresolved.includes("No relevant capability"));
    assert.doesNotMatch(unresolved, /establish a plan|support classifier/i);
    const afterEmpty = await partialRuntime.sessionManager.getByLogicalRef(partialRef);
    assert.equal(afterEmpty?.teachingPrompt, beforePartial.teachingPrompt);
    assert.deepEqual(afterEmpty?.waves, beforePartial.waves, "empty deltas do not invent teaching waves");
    assert.deepEqual(afterEmpty?.seeds, beforePartial.seeds);
    assert.equal(afterEmpty?.intentProvenance.nodes.at(-1)?.intent, "Find a related record");
    insufficient = false;
    routed.length = 0;
    const initial = "Only selected records may be changed.";
    const runtime = new AgentRuntime({ agentRoot: root, initialIntent: initial, engine, archive: null, hostTransport: null });
    const original = "Read the records matching my original selection.";
    const opened = await runtime.plasmContext({ intent: original,  });
    const ref = opened.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(ref);
    assert.equal((await runtime.sessionManager.getByLogicalRef(ref))?.intent, initial);
    await runtime.plasmContext({ intent: "Read related details.",  sessionMode: "extend", logicalSessionRef: ref });
    assert.deepEqual(routed.map(p => p.nodes.map(n => n.intent)), [[initial, original], [initial, original, "Read related details."]]);
    const second = await runtime.plasmContext({ intent: original,  });
    const secondRef = second.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(secondRef);
    assert.notEqual(secondRef, ref, "new workflows with the same intent remain distinct");
    assert.equal((await runtime.sessionManager.store.listSessions()).length, 3);
    await runtime.plasmContext({ intent: "Read more details.",  sessionMode: "extend", logicalSessionRef: ref });
    assert.deepEqual(routed.at(-1)?.nodes.map(n => n.intent), [initial, original, "Read related details.", "Read more details."]);
    insufficient = true;
    await runtime.plasmContext({ intent: "Resolve missing relation.",  sessionMode: "extend", logicalSessionRef: ref });
    insufficient = false;
    await runtime.plasmContext({ intent: "Read another relation.",  sessionMode: "extend", logicalSessionRef: ref });
    assert.deepEqual(routed.at(-1)?.nodes.map(n => n.intent), [initial, original, "Read related details.", "Read more details.", "Resolve missing relation.", "Read another relation."]);
    const stored = await runtime.sessionManager.store.get((await runtime.sessionManager.getByLogicalRef(ref))!.logicalSessionRef);
    assert.deepEqual(stored?.intentProvenance, routed.at(-1));
    insufficient = true;
    const missingIntent = "  Resolve only the selected records.  ";
    const missing = await runtime.plasmContext({ intent: missingIntent,  });
    assert.match(missing, /No relevant capability in this bounded packet/);
    const missingRef = missing.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(missingRef, "insufficient new discovery retains its session reference");
    insufficient = false;
    await runtime.plasmContext({ intent: "Resolve the related records.",  sessionMode: "extend", logicalSessionRef: missingRef });
    assert.deepEqual(routed.at(-1)?.nodes.map(n => n.intent), [initial, missingIntent, "Resolve the related records."]);
    const sealed: string[] = [];
    const evaluationEngine: PlasmEngine = { ...engine, learning: {
      configure: async () => {}, configureTraining: unused,
      sealTrainingEvaluation: async id => { sealed.push(id); },
      trainingStatus: unused, trainingCommand: unused, trainingCandidates: unused,
      drainSession: unused, releaseSession: unused, completedExecutions: unused,
      readProgram: unused, review: unused, fetchExample: unused,
    } };
    const evaluation = new AgentRuntime({ agentRoot: root, engine: evaluationEngine, archive: null, hostTransport: null,
      workflowLearning: { mode: "admission", scope: { tenant: "test", project: "test" }, databaseUrl: "postgresql://fixture" }, trainingEvaluation: true });
    insufficient = true;
    const noMatch = await evaluation.plasmContext({ intent: "Find an unavailable capability." });
    const evaluationRef = noMatch.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(evaluationRef);
    assert.equal(sealed.length, 0, "no native execution session exists for an empty discovery");
    await evaluation.plasmContext({ intent: "Still unavailable.", sessionMode: "extend", logicalSessionRef: evaluationRef });
    assert.equal(sealed.length, 0, "an empty extension must not seal a nonexistent execution session");
    insufficient = false;
    await evaluation.plasmContext({ intent: "Find an available capability.", sessionMode: "extend", logicalSessionRef: evaluationRef });
    assert.equal(sealed.length, 1, "evaluation isolation is sealed before exposing an executable session");
    await evaluation.plasmContext({ intent: "Find another capability.", sessionMode: "extend", logicalSessionRef: evaluationRef });
    assert.equal(sealed.length, 1, "an existing evaluation session is sealed once");
    await evaluation.close();
    let deliveries = 0;
    let recallReady = false;
    const deliveryEngine: PlasmEngine = { ...evaluationEngine,
      dryRun: async () => ({ writeCount: 1, planCommitRef: "pc1", summary: "Plan ready", compJson: "{}" }),
      takeCompletedTeaching: async (_id, includeExamples = true) => includeExamples && recallReady && ++deliveries <= 2 ? `Optional workflow examples for the original intent:\n- example ${deliveries}: Read records` : "",
    };
    const delivery = new AgentRuntime({ agentRoot: root, engine: deliveryEngine, archive: null, hostTransport: null,
      workflowLearning: { mode: "examples", scope: { tenant: "test", project: "test" }, databaseUrl: "postgresql://fixture" } });
    const deliveryContext = await delivery.plasmContext({ intent: "Read records" });
    const deliveryRef = deliveryContext.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(deliveryRef);
    recallReady = true;
    const deliveredDuringDiscovery = await delivery.plasmContext({ intent: "Find related records", sessionMode: "extend", logicalSessionRef: deliveryRef });
    assert.match(deliveredDuringDiscovery, /example 1: Read records/);
    const delivered = await delivery.plasm({ logicalSessionRef: deliveryRef, program: "example" });
    assert.match(delivered, /example 2: Read records/);
    const deliveredAgain = await delivery.plasm({ logicalSessionRef: deliveryRef, program: "example" });
    assert.doesNotMatch(deliveredAgain, /Optional workflow examples/);
    await delivery.close();
    const combinedEvents: string[] = [];
    const combinedEngine: PlasmEngine = {
      ...evaluationEngine,
      routeIntent: async (...args) => {
        combinedEvents.push("route");
        if (args[0].nodes.at(-1)?.intent === "Fail discovery") throw new Error("routing unavailable");
        return { ...await engine.routeIntent(...args),
          workflow_examples: [{ example_ref: randomUUID(), purpose: "Read records", outputs: [], assurance: "agent_attested" }] };
      },
      takeCompletedTeaching: async (_session, includeExamples) => {
        combinedEvents.push("poll");
        assert.equal(includeExamples, false, "already offered examples must not be repeated");
        return "Additional discovery for the current request:\ne1: RelatedRecord";
      },
    };
    const combined = new AgentRuntime({ agentRoot: root, engine: combinedEngine, archive: null, hostTransport: null,
      workflowLearning: { mode: "examples", scope: { tenant: "test", project: "test" }, databaseUrl: "postgresql://fixture" } });
    const combinedContext = await combined.plasmContext({ intent: "Read related records" });
    assert.match(combinedContext, /Optional workflow examples/);
    assert.match(combinedContext, /Additional discovery for the current request/);
    const combinedRef = combinedContext.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(combinedRef);
    combinedEvents.length = 0;
    const refined = await combined.plasmContext({ intent: "Refine related records", sessionMode: "extend", logicalSessionRef: combinedRef });
    assert.deepEqual(combinedEvents.slice(0, 2), ["poll", "route"], "ready guidance is collected before changing intent");
    assert.match(refined, /Completed discovery for the previous request/);
    await assert.rejects(
      combined.plasmContext({ intent: "Fail discovery", sessionMode: "extend", logicalSessionRef: combinedRef }),
      /Completed discovery for the previous request:[\s\S]*Current discovery failed: routing unavailable/,
    );
    await combined.close();
    const failedSealEngine: PlasmEngine = { ...evaluationEngine, learning: { ...evaluationEngine.learning!,
      sealTrainingEvaluation: async () => { throw new Error("evaluation seal failed"); },
    } };
    const failedSeal = new AgentRuntime({ agentRoot: root, engine: failedSealEngine, archive: null, hostTransport: null,
      workflowLearning: { mode: "admission", scope: { tenant: "test", project: "test" }, databaseUrl: "postgresql://fixture" }, trainingEvaluation: true });
    insufficient = true;
    const failedSealNoMatch = await failedSeal.plasmContext({ intent: "Find a missing capability." });
    const failedSealRef = failedSealNoMatch.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(failedSealRef);
    await assert.rejects(() => failedSeal.plasm({ logicalSessionRef: failedSealRef, program: "not executed" }), /evaluation seal failed/);
    insufficient = false;
    await assert.rejects(() => failedSeal.plasmContext({ intent: "Find an available capability." }), /evaluation seal failed/);
    await failedSeal.close();
    for (const invalid of ["\0", "\ud800"]) {
      const before = routed.length;
      await assert.rejects(() => runtime.plasmContext({ intent: `Read${invalid}`,  }));
      assert.equal(routed.length, before, "invalid text fails before native discovery");
    }
    const concurrentIntents = Array.from({ length: 12 }, (_, i) => `Resolve independent relation ${i}`);
    const concurrentResults = await Promise.allSettled(concurrentIntents.map((intent) => runtime.plasmContext({
      intent,  sessionMode: "extend", logicalSessionRef: ref,
    })));
    for (const result of concurrentResults) {
      if (result.status === "rejected") throw result.reason;
    }
    const afterConcurrent = await runtime.sessionManager.getByLogicalRef(ref);
    assert.ok(afterConcurrent);
    assert.deepEqual(afterConcurrent.intentProvenance.nodes.slice(-concurrentIntents.length).map(n => n.intent), concurrentIntents);
    const committed = provenanceBySession.get(afterConcurrent.logicalSessionId);
    assert.deepEqual(afterConcurrent.intentProvenance, committed, "filesystem and native provenance must agree");
    const beforeDuplicates = routed.length;
    const duplicateRequest = { intent: "Resolve the same relation",  sessionMode: "extend" as const, logicalSessionRef: ref };
    const duplicates = await Promise.all(Array.from({ length: 80 }, () =>
      runtime.plasmContext(JSON.parse(JSON.stringify(duplicateRequest)))));
    assert.equal(routed.length - beforeDuplicates, 1, "identical pending requests share one native discovery");
    assert.ok(duplicates.every(result => result === duplicates[0]));
    await runtime.plasmContext(duplicateRequest);
    assert.equal(routed.length - beforeDuplicates, 2, "completed discovery is not cached");
    const beforeDifferentNeeds = routed.length;
    await Promise.all(["Read left relation", "Read right relation"].map(intent =>
      runtime.plasmContext({ ...duplicateRequest, intent })));
    assert.equal(routed.length - beforeDifferentNeeds, 2, "different needs must never coalesce");
    const mutableRequest = { ...duplicateRequest };
    const pending = runtime.plasmContext(mutableRequest);
    mutableRequest.intent = "Changed after submission";
    await pending;
    assert.equal(routed.at(-1)?.nodes.at(-1)?.intent, duplicateRequest.intent);
    const newWorkflows = await Promise.all([0, 1].map(() => runtime.plasmContext({
      intent: "Same new workflow",
    })));
    assert.notEqual(newWorkflows[0]!.match(/l_[A-Za-z0-9_-]{22}/)?.[0], newWorkflows[1]!.match(/l_[A-Za-z0-9_-]{22}/)?.[0]);
    guidanceOnly = true;
    const repeated = await runtime.plasmContext({ ...duplicateRequest, intent: "Use existing declarations" });
    assert.match(repeated, /No new declarations/);
    assert.match(repeated, /Bind credential from the existing source/);
    assert.doesNotMatch(repeated, /Additional declarations/);
    guidanceOnly = false;
    const added = await runtime.plasmContext({ ...duplicateRequest, intent: "Find a new operation" });
    assert.match(added, /Additional declarations/);
    malformedTeaching = true;
    await assert.rejects(() => runtime.plasmContext({ intent: "Malformed initial teaching" }), /canonical teaching/);
    const beforeMalformed = await runtime.sessionManager.getByLogicalRef(ref);
    await assert.rejects(() => runtime.plasmContext({ intent: "Malformed incremental teaching", sessionMode: "extend", logicalSessionRef: ref }), /canonical teaching/);
    assert.deepEqual(await runtime.sessionManager.getByLogicalRef(ref), beforeMalformed);
    malformedTeaching = false;
    if (exercise) await exercise(runtime, ref, engine);
    console.log("session-extension: new -> persist -> concurrent extend passed");
  } finally {
    await rm(root, { recursive: true, force: true });
  }

}

if (process.argv[1]?.endsWith("test-session-extension.ts")) await checkSessionExtension();
