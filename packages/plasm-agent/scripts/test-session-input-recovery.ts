import assert from "node:assert/strict";
import { MockLanguageModelV3 } from "ai/test";
import { asSchema } from "ai";
import { checkSessionExtension } from "./test-session-extension.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { terminalExecutionFailure } from "../src/runtime/execution-failure.js";
import { newEphemeralLogicalSession } from "../src/runtime/logical-session.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";

await checkSessionExtension(async () => {}, async (runtime, ref, engine) => {
  let admissions = 0;
  engine.dryRun = async () => {
    admissions++;
    return { writeCount: 0, planCommitRef: "pc1", summary: "corrected reference admitted", fusedCleanRead: false };
  };
  const tools = createPlasmTools(runtime);
  for (const name of ["plasm_context", "plasm", "plasm_run"] as const) {
    const schema = asSchema(tools[name]!.inputSchema);
    assert.ok(schema.validate);
    const args = name === "plasm_context" ? { intent: "Read abstract records", session_mode: "extend" }
      : name === "plasm" ? { program: "fixture" } : { run_ref: "pc1" };
    assert.equal((await schema.validate({ ...args, logical_session_ref: ref })).success, true);
    assert.equal((await schema.validate({ ...args, logical_session_ref: ref.slice(0, -1) })).success, false);
    assert.ok(JSON.stringify(await schema.jsonSchema).includes('"pattern"'), "model sees the canonical reference shape");
  }
  const options = { toolCallId: "fixture", messages: [], context: {} };
  for (const invalid of ["for", ref.slice(0, -1), newEphemeralLogicalSession().logicalSessionRef]) {
    for (const name of ["plasm_context", "plasm", "plasm_run"] as const) {
      const args = name === "plasm_context"
        ? { intent: "Read abstract records", session_mode: "extend", logical_session_ref: invalid }
        : name === "plasm" ? { logical_session_ref: invalid, program: "fixture" }
        : { logical_session_ref: invalid, run_ref: "pc1" };
      const observed = await tools[name]!.execute!(args, options);
      assert.equal(typeof observed, "object");
      const value = observed as { failure: { cause: string; recovery: string; effects_unresolved: boolean; diagnostic: string } };
      assert.equal(value.failure.cause, "program");
      assert.equal(value.failure.recovery, "repair_program");
      assert.equal(value.failure.effects_unresolved, false);
      assert.ok(value.failure.diagnostic.includes(ref), "correction supplies the open workflow ref verbatim");
      assert.equal(terminalExecutionFailure([{ role: "tool", content: [{ toolName: name, output: { type: "json", value: observed } }] }]), undefined);
    }
  }
  assert.equal(admissions, 0, "bad handles never reach the engine");
  let requests = 0;
  const model = new MockLanguageModelV3({ doStream: async () => {
    requests++;
    const input = { logical_session_ref: requests === 1 ? ref.slice(0, -1) : ref, program: "fixture" };
    return { stream: new ReadableStream({ start(controller) {
      controller.enqueue({ type: "tool-call", toolCallId: `call-${requests}`, toolName: "plasm", input: JSON.stringify(input) });
      controller.enqueue({ type: "finish", finishReason: { unified: "tool-calls", raw: undefined }, usage: { inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 }, outputTokens: { total: 2, text: 2, reasoning: 0 } } });
      controller.close();
    } }) };
  } });
  const result = await runEveToolLoop({ model, system: "Abstract recovery fixture", messages: [{ role: "user", content: "Read abstract records" }], tools, maxSteps: 2, agentName: "session-input-recovery", telemetry: { isEnabled: false } });
  assert.equal(requests, 2, "invalid input buys a bounded correction generation");
  assert.equal(admissions, 1, "only the corrected call reaches the engine");
  assert.notEqual(result.stopReason, "execution_failed");
});
console.log("PASS: malformed and unknown session refs are effect-free corrections; the model loop admits a corrected call");
