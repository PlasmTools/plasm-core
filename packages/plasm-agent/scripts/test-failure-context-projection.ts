import assert from "node:assert/strict";
import { MockLanguageModelV3 } from "ai/test";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { AgentExecutionFailure, failureObservation, plasmModelOutput, terminalExecutionFailure, type ExecutionFailure } from "../src/runtime/execution-failure.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";

for (const recovery of ["reconcile_effects", "stop"] as const) {
  const failure: ExecutionFailure = {
    cause: "runtime", recovery, code: "fixture_failure", diagnostic: "write stopped",
    node: "__py1", occurrence_path: [2], catalog_digest: "fixture-private-digest",
    effects: [{entry_id: "matrix", capability: "write", completed: 1, failed: 0, occurrences: [{source_index: 0, source_identity: "Item/a", status: "completed"}]}],
    dispatches: [{operation: {entry_id: "matrix", capability: "write"}, request_fingerprint: "fixture-private-fingerprint", status: "unresolved"}],
    effects_unresolved: true,
  };
  const summary = `Execution stopped: write stopped.\nEffects: matrix/write: 1 completed; 1 unresolved.\n${recovery === "stop" ? "Stop this execution." : "Check completed and unresolved effects before another run; this run did not roll back."}`;
  let requests = 0;
  let traced = 0;
  const tools = createPlasmTools({
    plasmContext: async () => "context",
    plasm: async () => "plan",
    plasmRun: async () => {throw new AgentExecutionFailure(failure, summary);},
    recordExecutionFailure: async (_name, observed) => {assert.deepEqual(observed, failure);traced++;},
  });
  const model = new MockLanguageModelV3({doStream: async request => {
    requests++;
    if (requests === 2) {
      const prompt = JSON.stringify(request.prompt);
      assert.ok(prompt.includes("1 completed"));
      for (const hidden of ["fixture-private-digest", "fixture-private-fingerprint", "catalog_digest", "occurrence_path", "effects_unresolved"])
        assert.ok(!prompt.includes(hidden), `machine evidence leaked: ${hidden}`);
    }
    return {stream: new ReadableStream({start(controller) {
      if (requests === 1) controller.enqueue({type: "tool-call", toolCallId: "write", toolName: "plasm_run", input: JSON.stringify({logical_session_ref: "l_AAAAAAAAQACAAAAAAAAAAQ", run_ref: "pc1"})});
      else {
        controller.enqueue({type: "text-start", id: "text"});
        controller.enqueue({type: "text-delta", id: "text", delta: "Observed the stopped write."});
        controller.enqueue({type: "text-end", id: "text"});
      }
      controller.enqueue({type: "finish", finishReason: {unified: requests === 1 ? "tool-calls" : "stop", raw: undefined}, usage: {inputTokens: {total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0}, outputTokens: {total: 2, text: 2, reasoning: 0}}});
      controller.close();
    }})};
  }});
  const result = await runEveToolLoop({model, system: "Abstract failure projection fixture", messages: [{role: "user", content: "Perform the reviewed write"}], tools, maxSteps: 2, agentName: "failure-projection", telemetry: {isEnabled: false}});
  assert.equal(traced, 1, "full structured evidence retained by the trace seam");
  assert.equal(requests, recovery === "stop" ? 1 : 2);
  assert.equal(result.stopReason === "execution_failed", recovery === "stop");
  if (recovery === "stop") assert.deepEqual(result.executionFailure, failure);
}
assert.equal(terminalExecutionFailure([{toolName: "plasm", output: "execution_failed: stop"}]), undefined, "presentation cannot grant recovery authority");
const privateDiagnostic = '{"_ref":"private-fixture","catalog_digest":"private-digest","node":"__py72"}';
for (const error of [new Error(privateDiagnostic), privateDiagnostic]) {
  const observation = failureObservation(error);
  assert.equal(observation.failure.diagnostic, privateDiagnostic, "host evidence preserves the original diagnostic");
  assert.equal(observation.failure.recovery, "stop");
  assert.equal(observation.failure.effects_unresolved, true);
  const modelOutput = plasmModelOutput({output: observation});
  for (const hidden of ["_ref", "private-fixture", "catalog_digest", "__py72"])
    assert.ok(!modelOutput.value.includes(hidden), `unclassified evidence leaked: ${hidden}`);
  assert.ok(modelOutput.value.includes("Effects may be unresolved"));
}
console.log("PASS: SDK model context gets concise text; host recovery and trace retain full typed evidence");
