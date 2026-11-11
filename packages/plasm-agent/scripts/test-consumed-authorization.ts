import assert from "node:assert/strict";
import { executionFailureSchema, AgentExecutionFailure, failureObservation, terminalExecutionFailure } from "../src/runtime/execution-failure.js";
import { FailureResults } from "../src/tools/failure-results.js";

for (const state of ["Running", "Succeeded", "Failed", "Indeterminate"]) {
  const failure = executionFailureSchema.parse({
    cause: "authorization", recovery: "reconcile_effects", code: "plan_authorization_consumed",
    diagnostic: `authorization pc7 is ${state}; this call dispatched nothing.`,
    node: null, occurrence_path: [], catalog_digest: null,
    effects: [], dispatches: [], effects_unresolved: false,
  });
  const store = new FailureResults();
  const text = store.read(store.record(failure), 0);
  assert.match(text, /this call dispatched nothing/);
  assert.match(text, /A prior effect completed\. Check its receipt/);
  assert.doesNotMatch(text, /Correct the program|Revise the program|Obtain the required authorization/);
  const observation = failureObservation(new AgentExecutionFailure(failure, text));
  assert.deepEqual(observation.failure, failure);
  assert.equal(terminalExecutionFailure([{ toolName: "plasm_run", output: observation }]), undefined);
}
console.log("PASS: consumed authorization preserves typed recovery and remains recoverable without program-repair advice");
