import assert from "node:assert/strict";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { AgentExecutionFailure, executionFailureSchema, failureObservationSchema, plasmModelOutput, terminalExecutionFailure } from "../src/runtime/execution-failure.js";
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
  plasmContext: async () => "context", plasm: async () => "plan", plasmResume: async () => { throw new Error("unexpected repair"); },
  plasmRun: async () => { executions++; throw new AgentExecutionFailure(failure, "Execution stopped: status 422: Already set. 75 completed; earlier effects were not rolled back."); },
  recordExecutionFailure: async () => {},
};
const options = {toolCallId: "fixture", messages: [], context: {}};
const tools = createPlasmTools(runtime);
assert.deepEqual(Object.keys(tools).sort(), ["plasm", "plasm_context", "plasm_failure", "plasm_run"]);
{
  const paused = executionFailureSchema.parse({
    cause: "upstream", recovery: "replan_after_rejection", code: "upstream_rejection",
    diagnostic: "status 422: quota unavailable", node: null, occurrence_path: [], catalog_digest: null,
    effects_unresolved: false, effects: [],
    dispatches: [{operation: {entry_id: "matrix", capability: "change"}, request_fingerprint: "opaque", status: "rejected_without_effect"}],
  });
  const pausedTools = createPlasmTools({...runtime, plasmRun: async () => {
    throw new AgentExecutionFailure(paused, "Execution paused after a confirmed no-effect rejection. The original execution remains suspended and cannot be continued through the agent tools.");
  }});
  const output = await pausedTools.plasm_run!.execute!({logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", run_ref: "pc0"}, options);
  const text = plasmModelOutput({output}).value;
  assert.match(text, /status 422: quota unavailable/);
  assert.match(text, /remains suspended/);
  assert.doesNotMatch(text, /plasm_resume/);
}
{
  const small = executionFailureSchema.parse({cause: "program", recovery: "repair_program", code: "program_syntax",
    diagnostic: "Missing input", node: null, occurrence_path: [], catalog_digest: null,
    effects_unresolved: false, effects: [], dispatches: []});
  const smallTools = createPlasmTools({...runtime, plasm: async () => { throw new AgentExecutionFailure(small, "Program rejected."); }});
  const output = await smallTools.plasm!.execute!({logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", program: "example"}, options);
  assert.match(plasmModelOutput({output}).value, /Missing input/);
}
{
  const parseFailure = executionFailureSchema.parse({cause: "program", recovery: "repair_program", code: "program_syntax",
    diagnostic: "Expected an indented block", node: null, occurrence_path: [], catalog_digest: null,
    effects: [], dispatches: [], effects_unresolved: false});
  const parseTools = createPlasmTools({...runtime, plasm: async () => { throw new AgentExecutionFailure(parseFailure, "needs_fix · parse\n\nExpected an indented block"); }});
  const result = await parseTools.plasm!.execute!({logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", program: "class P(Program):\n    # incomplete"}, options);
  const text = plasmModelOutput({output: result}).value;
  assert.equal((text.match(/Expected an indented block/g) ?? []).length, 1);
  assert.doesNotMatch(text, /Recorded failure details/);
}
{
  const failure = executionFailureSchema.parse({cause: "program", recovery: "repair_program", code: "effect_plan_retention_refused",
    diagnostic: "continuation retained-state accounting failed", node: null, occurrence_path: [], catalog_digest: null,
    effects: [], dispatches: [], effects_unresolved: false});
  const noEffectTools = createPlasmTools({...runtime, plasmRun: async () => { throw new AgentExecutionFailure(failure, "Execution stopped: continuation retained-state accounting failed."); }});
  const result = await noEffectTools.plasm_run!.execute!({logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", run_ref: "pc0"}, options);
  const text = plasmModelOutput({output: result}).value;
  assert.equal((text.match(/continuation retained-state accounting failed/g) ?? []).length, 1);
  assert.doesNotMatch(text, /Recorded failure details/);
}
const output = await tools.plasm_run!.execute!({logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", run_ref: "pc0"}, options);
const visible = plasmModelOutput({output}).value;
assert.match(visible, /status 422: Already set/);
assert.match(visible, /75 completed/);
const ref = visible.match(/failure_[0-9a-f-]{36}/)?.[0];
assert.ok(ref, "model receives a usable retrieval reference");
assert.match(visible, /Recorded failure details \(1–40/);
const pages: string[] = [visible];
let offset = Number(visible.match(/and offset=(\d+)/)![1]);
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
assert.ok(!all.includes("upstream_rejection") && !all.includes("reconcile_effects"));
assert.match(all, /unattempted work is not listed/);
assert.equal((all.match(/— completed —/g) ?? []).length, 75);
for (let i = 0; i < 75; i++) assert.ok(all.includes(`item path [${i}] — Item:${i % 5} 漢字`));
assert.ok(all.includes("Failed input: Item:failed; item path [75]"));
assert.ok(all.includes("at first_write") && all.includes("at second_write"));
for (const hidden of ["__py72", "private-pin", "catalog_digest"]) assert.ok(!all.includes(hidden));
assert.equal(executions, 1, "reading evidence cannot repeat backend work");

// The same boundary delivers complete small failures for discovery, review and
// execution. No follow-up read is necessary to learn the recovery or effects.
for (const recovery of ["repair_program", "obtain_authorization", "reconcile_effects", "stop"] as const) {
  const small = executionFailureSchema.parse({...failure,
    cause: recovery === "repair_program" ? "program" : recovery === "obtain_authorization" ? "authorization" : "upstream",
    recovery, effects: recovery === "reconcile_effects" ? [{entry_id: "matrix", capability: "stage", completed: 1, failed: 0, occurrences: []}] : [], input_occurrence: undefined,
    effects_unresolved: recovery === "reconcile_effects",
    dispatches: recovery === "reconcile_effects" ? [{operation: {entry_id: "matrix", capability: "change"}, request_fingerprint: "opaque", status: "unresolved"}] : [],
  });
  for (const name of ["plasm_context", "plasm", "plasm_run"] as const) {
    let calls = 0;
    let recorded = 0;
    const fail = async () => { calls++; throw new AgentExecutionFailure(small, "Matrix operation stopped."); };
    const matrixTools = createPlasmTools({plasmContext: fail, plasm: fail, plasmRun: fail,
      recordExecutionFailure: async (_name, evidence) => { recorded++; assert.deepEqual(evidence, small); },
    });
    const args = name === "plasm_context" ? {session_mode: "extend", logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", intent: "Observe abstract records"}
      : name === "plasm" ? {logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", program: "abstract program"}
      : {logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", run_ref: "pc0"};
    const result = failureObservationSchema.parse(await matrixTools[name]!.execute!(args, options));
    assert.deepEqual(result.failure, small, "presentation does not change typed recovery authority");
    const text = plasmModelOutput({output: result}).value;
    if (small.effects_unresolved) assert.match(text, /End of recorded failure details/);
    else assert.doesNotMatch(text, /Recorded failure details/);
    assert.doesNotMatch(text, /call plasm_failure/);
    if (small.effects_unresolved) assert.match(text, /Earlier completed changes were not undone/);
    if (small.effects_unresolved) {
      assert.match(text, /matrix\/stage: 1 completed; 0 failed/);
      assert.match(text, /Some state changes remain unknown/);
      assert.match(text, /do not count it as another completed operation/);
    }
    assert.equal(calls, 1);
    assert.equal(recorded, 1);
  }
}
const successTools = createPlasmTools({plasmContext: async () => "declarations",
  plasm: async () => "review pc0", plasmRun: async () => "observed rows",
  recordExecutionFailure: async () => { assert.fail("success recorded as failure"); },
});
assert.equal(await successTools.plasm_context!.execute!({session_mode: "extend", logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", intent: "Read records"}, options), "declarations");
assert.equal(await successTools.plasm!.execute!({logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", program: "abstract"}, options), "review pc0");
assert.equal(await successTools.plasm_run!.execute!({logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", run_ref: "pc0"}, options), "observed rows");
const readFailure = new FailureResults();
{
  const record = executionFailureSchema.parse({...failure, effect_selections: [{
    collection: {catalog: Array(32).fill(1), expression: Array(32).fill(2), epoch: 3},
    coverage: "partial", count: 4, scope_path: ["selected"], occurrence_path: [],
  }]});
  const details = readFailure.read(readFailure.record(record), 0);
  assert.match(details, /4 materialized occurrences; source coverage partial/);
  assert.match(details, /does not establish that every selected operation ran/);
  assert.doesNotMatch(details, /catalog|expression|epoch/);
}
const lookupRef = readFailure.record({...failure, effects: [], input_occurrence: undefined,
  diagnostic: "Lookup rejected: account does not exist"});
const lookupDetails = readFailure.read(lookupRef, 0);
assert.match(lookupDetails, /Execution stopped/);
assert.match(lookupDetails, /Lookup rejected: account does not exist/);
assert.match(lookupDetails, /unattempted work is not listed/);
for (const status of ["unresolved", "response_received"] as const) {
  const record = executionFailureSchema.parse({...failure, effects: [], input_occurrence: undefined,
    effects_unresolved: true, dispatches: [{operation: {entry_id: "matrix", capability: "write"}, request_fingerprint: "opaque", status}]});
  const details = readFailure.read(readFailure.record(record), 0);
  assert.match(details, /does not establish whether state changed/);
  assert.match(details, /Check the resulting state before retrying/);
  assert.match(details, /do not count it as another completed operation/);
  assert.ok(!details.includes("occurrence []") && !details.includes("effect receipt"));
}
{
  const record = executionFailureSchema.parse({...failure, recovery: "replan_after_rejection", effects: [], input_occurrence: undefined,
    effects_unresolved: false, dispatches: [{operation: {entry_id: "matrix", capability: "write"}, request_fingerprint: "opaque", status: "rejected_without_effect"}]});
  const details = readFailure.read(readFailure.record(record), 0);
  assert.match(details, /rejected this invocation without effect/);
  assert.doesNotMatch(details, /Some state changes remain unknown/);
  assert.equal(terminalExecutionFailure([{toolName: "plasm_run", output: {status: "execution_failed", failure: record, summary: "rejected"}}]), undefined);
  const withReceipt = executionFailureSchema.parse({...record, effects: [{entry_id: "matrix", capability: "write", completed: 0, failed: 1, occurrences: []}]});
  assert.doesNotMatch(readFailure.read(readFailure.record(withReceipt), 0), /Failed calls may still have changed state/);
  const partial = executionFailureSchema.parse({...record, recovery: "reconcile_effects", effects: [{entry_id: "matrix", capability: "login", completed: 1, failed: 0, occurrences: []}]});
  const partialText = readFailure.read(readFailure.record(partial), 0);
  assert.match(partialText, /A prior effect completed/);
  assert.doesNotMatch(partialText, /unknown changes|state changes remain unknown/);
}
await assert.rejects(async () => createPlasmTools({...runtime}).plasm_failure!.execute!({failure_ref: ref, offset: 0}, options), /unknown or expired/);
await assert.rejects(async () => tools.plasm_failure!.execute!({failure_ref: ref, offset: 10000}, options), /outside/);
console.log("PASS: all partial results are accessible across turns, scoped to runtime, without backend calls or internal metadata");
