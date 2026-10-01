import assert from "node:assert/strict";
import { checkSessionExtension } from "./test-session-extension.js";
import { AgentExecutionFailure, failureObservation, terminalExecutionFailure, type ExecutionFailure } from "../src/runtime/execution-failure.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
const failure: ExecutionFailure = {
  cause: "upstream", recovery: "reconcile_effects", code: "upstream_rejection", diagnostic: "Already applied; retry immediately", node: "write", occurrence_path: [1], catalog_digest: null,
  effects: [{entry_id: "matrix", capability: "set_state", completed: 1, failed: 0, occurrences: [{source_index: 0, source_identity: "Item/a", status: "completed"}]}],
  dispatches: [], effects_unresolved: true,
};
const observation = failureObservation(new AgentExecutionFailure(failure));
assert.equal(observation.failure.diagnostic, failure.diagnostic);
assert.equal(observation.failure.recovery, "reconcile_effects");
assert(observation.recovery_instructions?.includes("A new execution is permitted"));
assert.equal(terminalExecutionFailure([{role: "tool", content: [{toolName: "plasm_run", output: {type: "json", value: observation}}]}]), undefined);
await checkSessionExtension(async () => {}, async (runtime, ref, engine) => {
  let calls = 0;
  let next = 0;
  engine.dryRun = async program => ({writeCount: program === "read" ? 0 : 1, planCommitRef: `pc${++next}`, summary: "0 writes: deliberately misleading prose", compJson: {}, fusedCleanRead: false});
  engine.runPlan = async runRef => {
    calls++;
    return runRef === "pc1" ? {ok: false, message: "opaque", failureJson: JSON.stringify(failure)} : {ok: true, message: "state observed", artifactsJson: "[]"};
  };
  await runtime.plasm({logicalSessionRef: ref, program: "write"});
  await assert.rejects(() => runtime.plasmRun({logicalSessionRef: ref, runRef: "pc1"}), AgentExecutionFailure);
  await runtime.plasm({logicalSessionRef: ref, program: "different write"});
  await runtime.plasmRun({logicalSessionRef: ref, runRef: `pc${next}`});
  assert.equal(calls, 2, "a failed execution does not block a new write execution");
  await runtime.plasm({logicalSessionRef: ref, program: "read"});
  await runtime.plasmRun({logicalSessionRef: ref, runRef: `pc${next}`});
  await runtime.plasm({logicalSessionRef: ref, program: "write after read"});
  await runtime.plasmRun({logicalSessionRef: ref, runRef: `pc${next}`});
  assert.equal(calls, 4, "reads and writes remain available without reconciliation gates");
  await runtime.plasm({logicalSessionRef: ref, program: "write"});
  await runtime.plasmRun({logicalSessionRef: ref, runRef: `pc${next}`});
  assert.equal(calls, 5, "identical program text in a new execution is not inferred replay");
});
await checkSessionExtension(async () => {}, async (runtime, ref, engine) => {
  let calls = 0;
  engine.dryRun = async () => ({writeCount: 1, planCommitRef: "pc-lost", summary: "write", compJson: {}, fusedCleanRead: false});
  engine.runPlan = async () => { calls++; throw new Error("lost host response"); };
  await runtime.plasm({logicalSessionRef: ref, program: "write"});
  await assert.rejects(() => runtime.plasmRun({logicalSessionRef: ref, runRef: "pc-lost"}));
  engine.runPlan = async () => { calls++; return {ok: true, message: "new execution", artifactsJson: "[]"}; };
  engine.dryRun = async () => ({writeCount: 1, planCommitRef: "pc-new", summary: "write", compJson: {}, fusedCleanRead: false});
  await runtime.plasm({logicalSessionRef: ref, program: "next write"});
  await runtime.plasmRun({logicalSessionRef: ref, runRef: "pc-new"});
  assert.equal(calls, 2, "a lost response does not poison subsequent executions");
});
// The tool wrapper must not blanket-lock discovery or inspection on reconciliation.
let reads = 0;
const tools = createPlasmTools({plasmContext: async () => "context", plasm: async () => {reads++; return "read plan";}, plasmRun: async () => {throw new AgentExecutionFailure(failure);}});
const options = {toolCallId: "fixture", messages: [], context: {}};
await tools.plasm_run!.execute!({logical_session_ref:"fixture",run_ref:"pc1"}, options);
await tools.plasm!.execute!({logical_session_ref:"fixture",program:"read"}, options);
assert.equal(reads, 1);
const fatalTools = createPlasmTools({plasmContext: async () => "context", plasm: async () => {reads++; return "new plan";}, plasmRun: async () => {throw new AgentExecutionFailure({...failure, recovery: "stop", cause: "runtime"});}});
await fatalTools.plasm_run!.execute!({logical_session_ref:"fixture",run_ref:"pc1"}, options);
await fatalTools.plasm!.execute!({logical_session_ref:"fixture",program:"new write"}, options);
assert.equal(reads, 2, "even terminal execution failures do not latch the session tool wrapper");

console.log("PASS: failure receipts remain visible; failed and interrupted executions do not block later reads or writes");
