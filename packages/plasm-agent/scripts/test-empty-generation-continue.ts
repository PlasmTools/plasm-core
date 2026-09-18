import assert from "node:assert/strict";
import { tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import { z } from "zod";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";
import { createEvalTerminalTools } from "../src/tools/harness-tools.js";
import { evalTerminalGrade } from "../src/tools/format.js";
import { toolInput } from "../src/tools/tool-input.js";

function modelFor(sequence: Array<"empty" | "write" | "done" | "prose">) {
  let calls = 0;
  const model = new MockLanguageModelV3({
    doStream: async () => ({
      stream: new ReadableStream({
        start(controller) {
          const action = sequence[calls++] ?? "empty";
          if (action === "empty") {
            controller.enqueue({ type: "reasoning-start", id: "r" });
            controller.enqueue({ type: "reasoning-delta", id: "r", delta: "Internal reasoning only" });
            controller.enqueue({ type: "reasoning-end", id: "r" });
          } else if (action === "prose") {
            controller.enqueue({ type: "text-start", id: "t" });
            controller.enqueue({ type: "text-delta", id: "t", delta: "Please clarify the request." });
            controller.enqueue({ type: "text-end", id: "t" });
          } else {
            controller.enqueue({ type: "tool-call", toolCallId: `call-${calls}`,
              toolName: action === "write" ? "write_effect" : "complete_task", input: "{}" });
          }
          controller.enqueue({ type: "finish",
            finishReason: { unified: action === "write" || action === "done" ? "tool-calls" : "stop", raw: "stop" },
            usage: {
              inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
              outputTokens: { total: 4109, text: action === "empty" ? 0 : 1, reasoning: 4109 },
            } });
          controller.close();
        },
      }),
    }),
  });
  return { model, calls: () => calls };
}

const recoveredModel = modelFor(["empty", "write", "empty", "done"]);
let writes = 0;
const recovered = await runEveToolLoop({
  model: recoveredModel.model, system: "system", agentName: "empty-response-recovery",
  messages: [{ role: "user", content: "perform the task" }], maxSteps: 4,
  telemetry: { isEnabled: false }, modelOptions: { maxOutputTokens: 16384 },
  tools: {
    write_effect: tool({ description: "Commit an effect", inputSchema: toolInput(z.object({})),
      execute: async () => { writes += 1; return "effect committed"; } }),
    ...createEvalTerminalTools(),
  },
});
assert.equal(recoveredModel.calls(), 4);
assert.equal(writes, 1, "recovery must not replay a committed effect");
assert.equal(recovered.stopReason, "completed");
assert.equal(recovered.lengthTruncationCount, 0);
assert.deepEqual(recovered.stepFinishReasons, ["stop", "tool-calls", "stop", "tool-calls"]);
assert.equal(evalTerminalGrade(recovered).kind, "null");
assert.equal(recovered.messages.filter(m => m.role === "user" && typeof m.content === "string"
  && m.content.includes("previous generation ended without text or a tool call")).length, 2);
assert.ok(recoveredModel.model.doStreamCalls.every(c => c.maxOutputTokens === 16384));

const exhaustedModel = modelFor(["empty"]);
const exhausted = await runEveToolLoop({
  model: exhaustedModel.model, system: "system", agentName: "empty-response-budget",
  messages: [{ role: "user", content: "perform the task" }], maxSteps: 3,
  telemetry: { isEnabled: false }, tools: createEvalTerminalTools(),
});
assert.equal(exhaustedModel.calls(), 3, "empty completions use the original step budget");
assert.equal(exhausted.stopReason, "budget_exhausted");
assert.equal(evalTerminalGrade(exhausted).kind, "unterminated");
assert.equal(exhausted.messages.filter(m => m.role === "user" && typeof m.content === "string"
  && m.content.includes("previous generation ended without text or a tool call")).length, 2,
  "no recovery request beyond the final allowed generation");

const proseModel = modelFor(["prose", "done"]);
const prose = await runEveToolLoop({
  model: proseModel.model, system: "system", agentName: "ordinary-prose-stop",
  messages: [{ role: "user", content: "perform the task" }], maxSteps: 3,
  telemetry: { isEnabled: false }, tools: createEvalTerminalTools(),
});
assert.equal(proseModel.calls(), 1, "ordinary prose is not an empty provider completion");
assert.equal(prose.stopReason, "unterminated");
console.log("ok: empty-generation-continue");
