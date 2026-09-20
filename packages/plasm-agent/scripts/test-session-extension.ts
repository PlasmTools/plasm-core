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
  let partial = false;
  const coverageBySession = new Map<string, Array<{ slot: { id: string; statement: string }; matched_capabilities: Array<{ catalog: string; capability: string }> }>>();
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
      const pin = sessionId ?? randomUUID();
      const coverage = structuredClone(coverageBySession.get(pin) ?? []);
      for (const statement of slots) {
        if (!coverage.some((entry) => entry.slot.statement === statement)) {
          coverage.push({ slot: { id: `s${coverage.length}`, statement }, matched_capabilities: [] });
        }
      }
      const active = coverage.filter((entry) => !entry.matched_capabilities.length || slots.includes(entry.slot.statement));
      const matches = active.map((entry, i) => {
        const direct = !insufficient && (!partial || i === 0);
        if (direct) entry.matched_capabilities = [{ catalog: "matrix", capability: "read" }];
        return { slot_id: entry.slot.id, capability_id: "matrix:read", choice: direct ? "direct_match" : "does_not_match",
          probabilities: { direct_match: direct ? 1 : 0, does_not_match: direct ? 0 : 1, uncertain: 0 }, confidence: 1 };
      });
      const unresolved = coverage.filter((entry) => !entry.matched_capabilities.length);
      const packet = routingPacketSchema.parse({
        routing: {
          intent_provenance: provenance, intent, pin_id: pin,
          authorization: { catalogs: ["matrix"], capabilities: {} },
          coverage: { obligations: coverage },
          retrieval: {
            generation: "fixture", candidates: [{
              id: "matrix:read", reference: { catalog: "matrix", capability: "read" },
              document: { entity: "Record" },
            }]
          },
          matching: {
            slots: active.map((entry) => entry.slot), matches,
            complete: matches.every((entry) => entry.choice === "direct_match"),
            unmatched_slot_ids: matches.filter((entry) => entry.choice !== "direct_match").map((entry) => entry.slot_id),
            additional_capability_ids: matches.some((entry) => entry.choice === "direct_match") ? ["matrix:read"] : [],
          },
          recovery: unresolved.length ? {
            unmatched_slots: unresolved.map((entry) => ({ slot_id: entry.slot.id, statement: entry.slot.statement,
              candidates: [{ reference: { catalog: "matrix", capability: "read" }, choice: "does_not_match", direct_match_probability: 0 }] })),
            available_catalogs: [], guidance: "Use available teaching while retaining unresolved obligations.",
          } : null,
          input_source_projection: [], input_source_matching: { matches: [], selected: [] },
          closure: insufficient ? null : { business: [{ catalog: "matrix", capability: "read" }], input_sources: [], prerequisites: [], acquisitions: [], edges: [] },
        },
        teaching: insufficient ? null : { tsv: "e1\tRecord", delta_refs: ["matrix:Record"] },
      });
      coverageBySession.set(pin, coverage);
      return packet;
    },
  };
  try {
    partial = true;
    const partialRuntime = new AgentRuntime({ agentRoot: root, engine, archive: null, hostTransport: null });
    const partiallyOpened = await partialRuntime.plasmContext({ intent: "Read selected records then publish them", effectSlots: ["Read selected records", "Publish selected records"] });
    assert.ok(partiallyOpened.includes("e1\tRecord"));
    assert.ok(partiallyOpened.includes("Publish selected records"));
    assert.ok(partiallyOpened.includes("rejected"));
    const partialRef = partiallyOpened.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(partialRef);
    const beforePartial = await partialRuntime.sessionManager.getByLogicalRef(partialRef);
    assert.ok(beforePartial);
    assert.ok(beforePartial.teachingTsv.includes("Record"));
    insufficient = true;
    const unresolved = await partialRuntime.plasmContext({ intent: "Find a related record", effectSlots: ["Read related records"], sessionMode: "extend", logicalSessionRef: partialRef });
    assert.ok(unresolved.includes("Publish selected records"), "omitted unresolved obligation remains visible");
    assert.equal((await partialRuntime.sessionManager.getByLogicalRef(partialRef))?.teachingTsv, beforePartial.teachingTsv);
    insufficient = false;
    partial = false;
    routed.length = 0;
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
    assert.equal((await runtime.sessionManager.store.listSessions()).length, 3);
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
