import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { jsonSchema, tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import { PlasmAgent } from "../src/runtime/plasm-agent.js";
import type { PlasmEngine } from "../src/engine/napi-binding.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";

const unused = async (): Promise<never> => { throw new Error("unexpected engine call"); };
const engine: PlasmEngine = {
  loadCatalog: unused, activateDiscovery: unused, routeIntent: unused,
  synthesizeTeaching: unused, dryRun: unused, runPlan: unused, run: unused,
  introspectCatalog: unused,
};
let count = 0;
const model = new MockLanguageModelV3({doStream: async () => {
  const index = count++;
  const isTool = index < 2;
  return {stream: new ReadableStream({start(controller) {
    if (isTool) controller.enqueue({type: "tool-call", toolCallId: `call-${index}`, toolName: "plasm_context", input: "{}"});
    else {
      controller.enqueue({type: "text-start", id: "text"});
      controller.enqueue({type: "text-delta", id: "text", delta: "final answer"});
      controller.enqueue({type: "text-end", id: "text"});
    }
    controller.enqueue({type: "finish", finishReason: {unified: isTool ? "tool-calls" : "stop", raw: undefined}, usage: {
      inputTokens: {total: 10, noCache: 10, cacheRead: 0, cacheWrite: 0},
      outputTokens: {total: 2, text: 2, reasoning: 0},
    }});
    controller.close();
  }})};
}});
const root = await mkdtemp(path.join(tmpdir(), "plasm-conversation-"));
try {
  const agent = new PlasmAgent({agentRoot: root, model, engine, hostTransport: null, archiveEnabled: false, telemetry: false});
  // This is the state which formerly removed context from the model's tool set.
  agent.runtime.hasOpenWorkflow = () => true;
  let executed = 0;
  const options = {wrapTools: () => ({plasm_context: tool({inputSchema: jsonSchema({type: "object", properties: {}}), execute: async () => `observation-${executed++}`})})};
  const first = await agent.generate("ORIGINAL_TASK", options);
  assert.equal(first.stopReason, "completed");
  assert.equal(first.usage.totalTokens, 36);
  assert.equal(executed, 2);
  for (const call of model.doStreamCalls) {
    assert.match(JSON.stringify(call.prompt), /ORIGINAL_TASK/);
    assert.ok(call.tools?.some(t => t.type === "function" && t.name === "plasm_context"));
  }
  const third = JSON.stringify(model.doStreamCalls[2]!.prompt);
  assert.equal((third.match(/observation-0/g) ?? []).length, 1);
  assert.equal((third.match(/observation-1/g) ?? []).length, 1);
  await agent.generate("FOLLOW_UP", options);
  const followup = JSON.stringify(model.doStreamCalls[3]!.prompt);
  for (const value of ["ORIGINAL_TASK", "FOLLOW_UP", "observation-0", "observation-1"]) assert.ok(followup.includes(value));
  count = 0;
  const bounded = await runEveToolLoop({model, system: "system", tools: options.wrapTools(), messages: [{role: "user", content: "BOUND"}], maxSteps: 1, agentName: "test", telemetry: {isEnabled: false}});
  assert.equal(bounded.stopReason, "budget_exhausted");
  assert.equal(bounded.text, "");
  await assert.rejects(runEveToolLoop({model, system: "", tools: {}, messages: [], maxSteps: NaN, agentName: "test"}), /positive integer/);
  console.log("PASS: multi-step and cross-turn history, context availability, usage, budget exhaustion");
} finally { await rm(root, {recursive: true, force: true}); }
