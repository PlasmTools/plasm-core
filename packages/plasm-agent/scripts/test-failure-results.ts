import assert from "node:assert/strict";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { AgentExecutionFailure, executionFailureSchema, plasmModelOutput } from "../src/runtime/execution-failure.js";
import { FailureResults } from "../src/tools/failure-results.js";

const failure = executionFailureSchema.parse({
  cause: "upstream", recovery: "reconcile_effects", code: "upstream_rejection",
  diagnostic: "status 422: Already set", node: "__py72", occurrence_path: [75],
  catalog_digest: "private-pin", effects_unresolved: false, dispatches: [],
  input_occurrence: {scope_path: ["__py72"], occurrence_path: [75], source_index: 75, source_identity: "Item:failed"},
  effects: [{entry_id: "matrix", capability: "write", completed: 75, failed: 0,
    occurrences: Array.from({length: 75}, (_, i) => ({scope_path: ["__py72", i % 2 ? "first_write" : "second_write"], occurrence_path: [i], source_index: i,
      source_identity: `Item:${i % 5} 漢字`, status: "completed"}))}],
});
let executions = 0;
const runtime = {
  plasmContext: async () => "context", plasm: async () => "plan",
  plasmRun: async () => { executions++; throw new AgentExecutionFailure(failure, "Execution stopped: status 422: Already set. 75 completed; earlier effects were not rolled back."); },
  recordExecutionFailure: async () => {},
};
const options = {toolCallId: "fixture", messages: [], context: {}};
const tools = createPlasmTools(runtime);
const output = await tools.plasm_run!.execute!({logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", run_ref: "pc0"}, options);
const visible = plasmModelOutput({output}).value;
assert.match(visible, /status 422: Already set/);
assert.match(visible, /75 completed/);
const ref = visible.match(/failure_[0-9a-f-]{36}/)?.[0];
assert.ok(ref, "model receives a usable retrieval reference");
const pages: string[] = [];
let offset = 0;
for (;;) {
  // Rebuilding tools between turns must not orphan the evidence.
  const page = String(await createPlasmTools(runtime).plasm_failure!.execute!({failure_ref: ref, offset}, options));
  pages.push(page);
  const next = page.match(/and offset=(\d+)/);
  if (!next) break;
  offset = Number(next[1]);
}
const all = pages.join("\n");
assert.match(pages[0]!, /status 422: Already set/);
assert.match(pages[0]!, /upstream_rejection/);
assert.match(all, /not an exhaustive list of unattempted work/);
assert.equal((all.match(/— completed —/g) ?? []).length, 75);
for (let i = 0; i < 75; i++) assert.ok(all.includes(`occurrence [${i}] — Item:${i % 5} 漢字`));
assert.ok(all.includes("Primary failure input occurrence [75]: Item:failed"));
assert.ok(all.includes("at first_write") && all.includes("at second_write"));
for (const hidden of ["__py72", "private-pin", "catalog_digest"]) assert.ok(!all.includes(hidden));
assert.equal(executions, 1, "reading evidence cannot repeat backend work");
const readFailure = new FailureResults();
const lookupRef = readFailure.record({...failure, effects: [], input_occurrence: undefined,
  diagnostic: "Lookup rejected: account does not exist"});
const lookupDetails = readFailure.read(lookupRef, 0);
assert.match(lookupDetails, /Execution stopped/);
assert.match(lookupDetails, /Lookup rejected: account does not exist/);
assert.match(lookupDetails, /not an exhaustive list of unattempted work/);
await assert.rejects(async () => createPlasmTools({...runtime}).plasm_failure!.execute!({failure_ref: ref, offset: 0}, options), /unknown or expired/);
await assert.rejects(async () => tools.plasm_failure!.execute!({failure_ref: ref, offset: 10000}, options), /outside/);
console.log("PASS: all partial results are accessible across turns, scoped to runtime, without backend calls or internal metadata");
