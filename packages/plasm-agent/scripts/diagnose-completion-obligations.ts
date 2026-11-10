/** Offline boundary reproducer. No model, transport, catalog, or credentials.
 * Default: fail if a terminal can discard an unresolved effect obligation.
 * --observe: print all verdicts without failing (diagnostic inventory only).
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { MockLanguageModelV3 } from "ai/test";
import { AgentExecutionFailure, executionFailureSchema } from "../src/runtime/execution-failure.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { createHarnessTools } from "../src/tools/harness-tools.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";

const syntheticFailure = {
  cause: "upstream", recovery: "reconcile_effects", code: "upstream_rejection",
  diagnostic: "One invocation rejected after an earlier invocation completed",
  node: "write", occurrence_path: [1], catalog_digest: null,
  effects: [{entry_id: "matrix", capability: "write", completed: 1, failed: 1,
    occurrences: [
      {scope_path: ["write"], occurrence_path: [0], source_index: 0, source_identity: "Item/a", status: "completed"},
      {scope_path: ["write"], occurrence_path: [1], source_index: 1, source_identity: "Item/b", status: "failed"},
    ]}],
  dispatches: [], effects_unresolved: false,
};
const nativePath = process.env.PLASM_EFFECT_OBSERVATION_PATH;
const failure = executionFailureSchema.parse(nativePath ? JSON.parse(readFileSync(nativePath, "utf8")) : syntheticFailure);
// Known outcomes are not the same as a completed selection: b failed and later
// members were never visited. Do not gate solely on effects_unresolved.
const session = "l_AAAAAAAAQACAAAAAAAAAAQ";
const cases = [
  {name: "failure_then_complete", terminal: "complete_task", intervening: "none"},
  {name: "failure_then_answer", terminal: "submit_answer", intervening: "none"},
  {name: "failure_then_unrelated_read_then_complete", terminal: "complete_task", intervening: "read"},
  {name: "failure_then_unrelated_write_then_complete", terminal: "complete_task", intervening: "write"},
] as const;
let exposed = 0;
for (const scenario of cases) {
  let generation = 0;
  let failuresRecorded = 0;
  let executions = 0;
  const runtime = {
    plasmContext: async () => "Workflow opened",
    plasm: async () => "Read-only observation; no reconciliation evidence",
    plasmRun: async () => {
      if (executions++ > 0) return "Another reviewed selection completed; no relation to the failed selection is certified";
      throw new AgentExecutionFailure(failure,
        "Execution failed after partial effects. Reconcile before completing.");
    },
    recordExecutionFailure: async () => { failuresRecorded++; },
  };
  const model = new MockLanguageModelV3({doStream: async () => {
    const step = generation++;
    const name = step === 0 ? "plasm_run"
      : scenario.intervening === "read" && step === 1 ? "plasm"
      : scenario.intervening === "write" && step === 1 ? "plasm_run" : scenario.terminal;
    const input = name === "plasm_run" ? {logical_session_ref: session, run_ref: step === 0 ? "pc0" : "pc1"}
      : name === "plasm" ? {logical_session_ref: session, program: "class P(Program):\n    def build(self):\n        return 1"}
      : name === "submit_answer" ? {answer: "42"} : {};
    return {stream: new ReadableStream({start(controller) {
      controller.enqueue({type: "tool-call", toolCallId: `call-${step}`, toolName: name, input: JSON.stringify(input)});
      controller.enqueue({type: "finish", finishReason: {unified: "tool-calls", raw: undefined},
        usage: {inputTokens: {total: 0, noCache: 0, cacheRead: 0, cacheWrite: 0}, outputTokens: {total: 0, text: 0, reasoning: 0}}});
      controller.close();
    }})};
  }});
  const result = await runEveToolLoop({model, system: "Abstract completion contract",
    messages: [{role: "user", content: "Perform the selected writes"}],
    tools: {...createPlasmTools(runtime), ...createHarnessTools({includeEvalTerminals: true,
      discoveryCompleted: () => true,
      responseObligation: {resolve: async () => scenario.terminal === "submit_answer" ? "value_required" : "no_answer"}})},
    maxSteps: scenario.intervening !== "none" ? 3 : 2, agentName: "completion-contract",
    telemetry: {isEnabled: false}});
  assert.equal(failuresRecorded, 1, "must exercise structured failure recording");
  const violated = result.stopReason === "completed";
  exposed += Number(violated);
  console.log(JSON.stringify({case: scenario.name, stopReason: result.stopReason,
    contract: violated ? "VIOLATED" : "not_completed", failuresRecorded}));
}
if (!process.argv.includes("--observe")) {
  assert.equal(exposed, 0, "unresolved failed selections must not become completed tasks");
}
