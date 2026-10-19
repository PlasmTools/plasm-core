import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { AgentExecutionFailure, executionFailureSchema, failureObservation, terminalExecutionFailure } from "../src/runtime/execution-failure.js";

const fixture = process.argv[2] ?? new URL("./fixtures/native-failure-contract.json", import.meta.url);
const failures: unknown[] = JSON.parse(readFileSync(fixture, "utf8"));
assert.equal(failures.length, 15);
for (const wire of failures) {
  const failure = executionFailureSchema.parse(wire);
  assert.deepEqual(failure, wire, "decoder preserves native evidence exactly");
  const observation = failureObservation(new AgentExecutionFailure(failure, "Original actionable diagnostic"));
  assert.deepEqual(observation.failure, wire);
  if (failure.cause === "program") {
    assert.equal(failure.recovery, "repair_program");
    assert.equal(terminalExecutionFailure([{toolName: "plasm", output: observation}]), undefined);
  }
  assert.equal(executionFailureSchema.safeParse({...failure, unexpected: true}).success, false);
}
const base = executionFailureSchema.parse(failures[0]);
for (const detail of [
  {kind: "unregistered"},
  {kind: "compute_inference", diagnostics: [{code: "x"}]},
  {kind: "compute_inference", diagnostics: [{code: "x", message: "x", unmaterializable_type: "invented", compute_line: null, compute_column: null, source_excerpt: null}]},
]) assert.equal(executionFailureSchema.safeParse({...base, program_rejection: {category: "type", span_offset: null, detail}}).success, false);
console.log("PASS: all native diagnostic variants preserve evidence and repair authority; strict invalid payloads rejected");
