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

export async function checkSessionExtension(observe: (intent: string) => Promise<void> = async () => { }): Promise<void> {
  const root = await mkdtemp(path.join(tmpdir(), "plasm-extension-"));
  const routed: IntentProvenance[] = [];
  let insufficient = false;
  const provenanceBySession = new Map<string, IntentProvenance>();
  const unused = async (): Promise<never> => { throw new Error("unexpected engine operation"); };
  const engine: PlasmEngine = {
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
      const matches=[{capability_id:"matrix:read",choice:insufficient?"unrelated":"relevant",probabilities:{relevant:insufficient?0:1,unrelated:insufficient?1:0,uncertain:0},confidence:1}];
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
          recovery: insufficient ? {candidates:[{reference:{catalog:"matrix",capability:"read"},choice:"unrelated",relevance_probability:0}],available_catalogs:[],guidance:"No relevant capability in this bounded packet."}:null,
          closure: insufficient ? null : { business: [{ catalog: "matrix", capability: "read" }], input_sources: [], prerequisites: [], acquisitions: [], edges: [] },
        },
        teaching: insufficient ? null : { tsv: "e1\tRecord", delta_refs: ["matrix:Record"] },
      });
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
    const partialRef = partiallyOpened.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(partialRef);
    const beforePartial = await partialRuntime.sessionManager.getByLogicalRef(partialRef);
    assert.ok(beforePartial);
    assert.ok(beforePartial.teachingTsv.includes("Record"));
    insufficient = true;
    const unresolved = await partialRuntime.plasmContext({ intent: "Find a related record",  sessionMode: "extend", logicalSessionRef: partialRef });
    assert.ok(unresolved.includes("No relevant capability"));
    assert.equal((await partialRuntime.sessionManager.getByLogicalRef(partialRef))?.teachingTsv, beforePartial.teachingTsv);
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
    const missingRef = missing.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(missingRef, "insufficient new discovery retains its session reference");
    insufficient = false;
    await runtime.plasmContext({ intent: "Resolve the related records.",  sessionMode: "extend", logicalSessionRef: missingRef });
    assert.deepEqual(routed.at(-1)?.nodes.map(n => n.intent), [initial, missingIntent, "Resolve the related records."]);
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
    console.log("session-extension: new -> persist -> concurrent extend passed");
  } finally {
    await rm(root, { recursive: true, force: true });
  }

}

if (process.argv[1]?.endsWith("test-session-extension.ts")) await checkSessionExtension();
