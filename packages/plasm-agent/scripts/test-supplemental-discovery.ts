import assert from "node:assert/strict";
import { MockLanguageModelV3 } from "ai/test";
import { createEvalTerminalTools } from "../src/tools/harness-tools.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";

let polls = 0;
let observed = false;
const model = new MockLanguageModelV3({
  doStream: async (options) => {
    observed = JSON.stringify(options.prompt).includes("supplemental declaration witness");
    return { stream: new ReadableStream({ start(controller) {
      controller.enqueue({ type: "tool-call", toolCallId: "finish", toolName: "complete_task", input: "{}" });
      controller.enqueue({ type: "finish", finishReason: { unified: "tool-calls", raw: undefined },
        usage: { inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 }, outputTokens: { total: 2, text: 2, reasoning: 0 } } });
      controller.close();
    } }) };
  },
});
const result = await runEveToolLoop({
  model, system: "system", tools: createEvalTerminalTools({ discoveryCompleted: () => true, sealEffectWorkflows: async () => {} }),
  messages: [{ role: "user", content: "task" }], maxSteps: 2, agentName: "supplemental-test",
  telemetry: { isEnabled: false },
  pollSessionUpdate: async () => ++polls === 1 ? "supplemental declaration witness" : "",
});
assert.ok(observed, "completed discovery must reach the next model request");
assert.ok(JSON.stringify(result.messages).includes("supplemental declaration witness"));
console.log("supplemental discovery reaches model history");
