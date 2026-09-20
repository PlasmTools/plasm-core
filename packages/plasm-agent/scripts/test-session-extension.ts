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
  const unused = async (): Promise<never> => { throw new Error("unexpected engine operation"); };
  const engine: PlasmEngine = {
    loadCatalog: unused, synthesizeTeaching: unused, dryRun: unused,
    activateDiscovery: unused, runPlan: unused, run: unused, introspectCatalog: unused,
    async routeIntent(provenance, slots, sessionId) {
      const intent = provenance.nodes.at(-1)!.intent;
      // PostgreSQL rejects a NUL in the text-search query parameter. Exercise the
      // real runtime + filesystem path before crossing this native boundary.
      assert.ok(!intent.includes("\0"), "discovery intent contains a storage-key NUL");
      await observe(intent);
      routed.push(provenance);
      const packet = routingPacketSchema.parse({
        routing: {
          intent_provenance: provenance, intent, pin_id: sessionId ?? randomUUID(),
          retrieval: {
            generation: "fixture", candidates: [{
              id: "matrix:read", reference: { catalog: "matrix", capability: "read" },
              document: { entity: "Record" },
            }]
          },
          matching: {
            slots: slots.map((statement, i) => ({ id: `s${i}`, statement })),
            matches: slots.map((_, i) => ({
              slot_id: `s${i}`, capability_id: "matrix:read",
              choice: "direct_match", probabilities: { direct_match: 1, does_not_match: 0, uncertain: 0 }, confidence: 1
            })),
            complete: true, unmatched_slot_ids: [], additional_capability_ids: ["matrix:read"],
          },
          input_source_projection: [], input_source_matching: { matches: [], selected: [] },
          closure: { business: [{ catalog: "matrix", capability: "read" }], input_sources: [], prerequisites: [], acquisitions: [], edges: [] },
        },
        teaching: { tsv: "e1\tRecord", delta_refs: ["matrix:Record"] },
      });
      if (insufficient) {
        packet.routing.closure = null;
        packet.routing.matching.complete = false;
        packet.routing.matching.matches = [];
        packet.routing.matching.unmatched_slot_ids = slots.map((_, i) => `s${i}`);
        packet.teaching = null;
      }
      return packet;
    },
  };
  try {
    const initial = "Only selected records may be changed.";
    const runtime = new AgentRuntime({ agentRoot: root, initialIntent: initial, engine, archive: null, hostTransport: null });
    const original = "Read the records matching my original selection.";
    const opened = await runtime.plasmContext({ intent: original, effectSlots: ["Read records"] });
    const ref = opened.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(ref);
    assert.equal((await runtime.sessionManager.getByLogicalRef(ref))?.intent, initial);
    await runtime.plasmContext({ intent: "Read related details.", effectSlots: ["Read details"], sessionMode: "extend", logicalSessionRef: ref });
    assert.deepEqual(routed.map(p => p.nodes.map(n => n.intent)), [[initial, original], [initial, original, "Read related details."]]);
    const second = await runtime.plasmContext({ intent: original, effectSlots: ["Read records"] });
    const secondRef = second.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(secondRef);
    assert.notEqual(secondRef, ref, "new workflows with the same intent remain distinct");
    assert.equal((await runtime.sessionManager.store.listSessions()).length, 2);
    await runtime.plasmContext({ intent: "Read more details.", effectSlots: ["Read details"], sessionMode: "extend", logicalSessionRef: ref });
    assert.deepEqual(routed.at(-1)?.nodes.map(n => n.intent), [initial, original, "Read related details.", "Read more details."]);
    insufficient = true;
    await runtime.plasmContext({ intent: "Resolve missing relation.", effectSlots: ["Read relation"], sessionMode: "extend", logicalSessionRef: ref });
    insufficient = false;
    await runtime.plasmContext({ intent: "Read another relation.", effectSlots: ["Read relation"], sessionMode: "extend", logicalSessionRef: ref });
    assert.deepEqual(routed.at(-1)?.nodes.map(n => n.intent), [initial, original, "Read related details.", "Read more details.", "Resolve missing relation.", "Read another relation."]);
    const stored = await runtime.sessionManager.store.get((await runtime.sessionManager.getByLogicalRef(ref))!.logicalSessionRef);
    assert.deepEqual(stored?.intentProvenance, routed.at(-1));
    insufficient = true;
    const missingIntent = "  Resolve only the selected records.  ";
    const missing = await runtime.plasmContext({ intent: missingIntent, effectSlots: ["Read unknown relation"] });
    const missingRef = missing.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(missingRef, "insufficient new discovery retains its session reference");
    insufficient = false;
    await runtime.plasmContext({ intent: "Resolve the related records.", effectSlots: ["Read relation"], sessionMode: "extend", logicalSessionRef: missingRef });
    assert.deepEqual(routed.at(-1)?.nodes.map(n => n.intent), [initial, missingIntent, "Resolve the related records."]);
    for (const invalid of ["\0", "\ud800"]) {
      const before = routed.length;
      await assert.rejects(() => runtime.plasmContext({ intent: `Read${invalid}`, effectSlots: ["Read records"] }));
      assert.equal(routed.length, before, "invalid text fails before native discovery");
    }
    console.log("session-extension: new -> persist -> extend passed");
  } finally {
    await rm(root, { recursive: true, force: true });
  }

}

if (process.argv[1]?.endsWith("test-session-extension.ts")) await checkSessionExtension();
