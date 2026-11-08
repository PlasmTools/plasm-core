/** Exercise actual runtime response assembly; experimental wording stays test-only. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { AgentRuntime } from "../src/runtime/agent-runtime.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { decodeRoutingPacket, type RoutingPacket } from "../src/engine/routing.js";

const fixture = decodeRoutingPacket(readFileSync(new URL("../../../fixtures/discovery/partial-routing.json", import.meta.url), "utf8"));
const cases = ["new_declarations", "unchanged", "unexplored", "byte_deferred",
  "negative_packet", "repeated_negative", "changed_request", "native_recovery"] as const;
type Case = typeof cases[number];
const other = { catalog: "matrix", capability: "other_read" };
const key = (ref: { catalog: string; capability: string }): string => JSON.stringify([ref.catalog, ref.capability]);

function packetFor(id: Case): RoutingPacket {
  const packet = structuredClone(fixture);
  const { routing } = packet;
  routing.search.exposed = fixture.routing.retrieval.candidates.map(c => c.reference);
  routing.retrieval.candidates = [];
  routing.matching.matches = [];
  routing.search.considered = [];
  routing.search.unexplored = [];
  routing.search.deferred = [];
  routing.search.requests = 0;
  routing.search.input_bytes = 0;
  routing.search.reported_input_tokens = 0;
  routing.search.missing_usage = 0;
  routing.closure = { business: [], input_sources: [], prerequisites: [], acquisitions: [], edges: [] };
  routing.recovery = null;
  packet.teaching = { prompt: "", declarations_changed: false, delta_refs: [] };
  if (["unexplored", "byte_deferred", "changed_request"].includes(id)) {
    routing.search.unexplored = [other];
    if (id !== "unexplored") routing.search.deferred = [{ reference: other, reason: "input_bytes" }];
  }
  if (["new_declarations", "negative_packet", "repeated_negative", "native_recovery"].includes(id)) {
    const c = structuredClone(fixture.routing.retrieval.candidates[0]!);
    c.id = "matrix/other_read";
    c.reference = other;
    routing.retrieval.candidates = [c];
    routing.search.considered = [other];
    routing.search.requests = 1;
    routing.search.input_bytes = 1000;
    routing.search.reported_input_tokens = 200;
    routing.matching.matches = [{ capability_id: c.id, choice: id === "new_declarations" ? "relevant" : "unrelated" }];
    if (id === "new_declarations") {
      routing.closure.business = [other];
      packet.teaching = { prompt: "# matrix::OtherRecord\ne2:\n    value: str\n", declarations_changed: true, delta_refs: ["matrix:OtherRecord"] };
    } else if (id === "native_recovery") {
      routing.search.exposed = [];
      routing.search.unexplored = [{ catalog: "matrix", capability: "unexamined_read" }];
      routing.closure = null;
      packet.teaching = null;
    }
    if (id === "native_recovery") routing.recovery = {
      candidates: [{ reference: other, choice: "unrelated" }], available_catalogs: [],
      guidance: "No contribution was retained in this packet. Other candidates remain unexamined.",
    };
  }
  return decodeRoutingPacket(JSON.stringify(packet));
}

function candidateText(packet: RoutingPacket, baseline: string, examined: ReadonlySet<string>): string {
  const { routing, teaching } = packet;
  const judged = routing.retrieval.candidates.filter(c => routing.matching.matches.some(m => m.capability_id === c.id));
  const repeated = judged.filter(c => examined.has(key(c.reference))).length;
  const retained = routing.matching.matches.filter(m => m.choice !== "unrelated").length;
  const deferred = routing.search.deferred.filter(d => d.reason === "input_bytes").length;
  const facts = [
    teaching?.declarations_changed ? "This call added declarations." : "This call added no declarations.",
    `This packet judged ${judged.length} capabilities and retained ${retained} contribution judgments.`,
    `${judged.length - repeated} capabilities were newly examined; ${repeated} were examined on earlier calls. Earlier judgments are not permanent exclusions.`,
    `${routing.search.unexplored.length} capabilities remain unexplored in this receipt.`,
    `${deferred} candidates were deferred for the packet byte budget. Deferral is not a judgment of unrelatedness.`,
    "These facts do not establish catalogue absence, successful execution, or task completion.",
  ];
  // Change wording only, leaving native declarations, references and recovery intact.
  const text = baseline.replace(
    "Use declared reads in a Program for current data; extend discovery when capabilities are missing or the need changes.",
    "Declared reads can obtain current data. This receipt does not guarantee that another discovery call will expose additional capabilities.",
  );
  return facts.join("\n") + "\n\n" + text;
}

const rows: Array<{
  id: Case;
  branch: "teaching" | "no_closure";
  baseline: string;
  candidate: string;
  receipt: RoutingPacket;
}> = [];
for (const id of cases) {
  const root = await mkdtemp(path.join(tmpdir(), "plasm-wording-offline-"));
  const expected = packetFor(id);
  let calls = 0;
  let forbiddenCalls = 0;
  const unused = async (): Promise<never> => { forbiddenCalls++; throw new Error("Unexpected execution or provider operation"); };
  const engine: PlasmEngine = { sealEffectWorkflows: async () => {}, resumeEffectRun: async () => { throw new Error("unexpected repair"); }, takeCompletedTeaching: async () => "",
    close: async () => {}, loadCatalog: unused, activateDiscovery: unused,
    synthesizeTeaching: unused, dryRun: unused, runPlan: unused, run: unused, introspectCatalog: unused,
    routeIntent: async (provenance, sessionId) => {
      const packet = structuredClone(calls++ === 0 && id !== "native_recovery" ? fixture : expected);
      packet.routing.intent_provenance = provenance;
      packet.routing.intent = provenance.nodes.at(-1)!.intent;
      if (sessionId) packet.routing.pin_id = sessionId;
      return decodeRoutingPacket(JSON.stringify(packet));
    },
  };
  const runtime = new AgentRuntime({ agentRoot: root, engine, archive: null, hostTransport: null });
  try {
    const initial = await runtime.plasmContext({ intent: "Inspect selected records." });
    const ref = initial.match(/l_[A-Za-z0-9_-]{22}/)?.[0];
    assert.ok(ref);
    const intent = id === "changed_request" ? "Publish selected records after inspection." : "Inspect selected records.";
    const history = new Set<string>();
    if (id === "repeated_negative") {
      await runtime.plasmContext({ intent, sessionMode: "extend", logicalSessionRef: ref });
      history.add(key(other));
    }
    const baseline = id === "native_recovery" ? initial : await runtime.plasmContext({ intent, sessionMode: "extend", logicalSessionRef: ref });
    const candidate = candidateText(expected, baseline, history);
    assert.ok(candidate.includes(ref));
    if (expected.teaching?.prompt) assert.ok(candidate.includes(expected.teaching.prompt.trim()));
    if (expected.routing.recovery) assert.ok(candidate.includes(expected.routing.recovery.guidance));
    assert.equal(forbiddenCalls, 0);
    assert.equal(calls, id === "native_recovery" ? 1 : id === "repeated_negative" ? 3 : 2);
    rows.push({ id, branch: expected.routing.closure ? "teaching" : "no_closure", baseline, candidate, receipt: expected });
  } finally {
    await runtime.close();
    await rm(root, { recursive: true, force: true });
  }
}
const find = (id: Case) => rows.find(row => row.id === id)!;
assert.equal(find("unexplored").baseline, find("byte_deferred").baseline);
assert.notEqual(find("unexplored").candidate, find("byte_deferred").candidate);
assert.equal(find("negative_packet").baseline, find("repeated_negative").baseline);
assert.notEqual(find("negative_packet").candidate, find("repeated_negative").candidate);
assert.equal(find("byte_deferred").candidate, find("changed_request").candidate,
  "Changed request text alone must not invent semantic progress or a new judgment");
assert.ok(find("native_recovery").baseline.includes("Other candidates remain unexamined"));
const invalid = packetFor("byte_deferred");
invalid.routing.search.unexplored = [];
assert.throws(() => decodeRoutingPacket(JSON.stringify(invalid)));
console.log(JSON.stringify({ providerCalls: 0, runtimeCases: rows.length, controlsPassed: true,
  baselineDistinct: new Set(rows.map(row => row.baseline)).size,
  candidateDistinct: new Set(rows.map(row => row.candidate)).size,
  limitation: "Synthetic receipts through the real runtime. No selection, semantic retrieval, model comprehension or agent-success measurement.", rows }, null, 2));
