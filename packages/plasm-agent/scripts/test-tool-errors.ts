import assert from "node:assert/strict";
import { tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import { z } from "zod";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";
import { createEvalTerminalTools } from "../src/tools/harness-tools.js";
import { toolInput } from "../src/tools/tool-input.js";

const usage = { inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
  outputTokens: { total: 2, text: 2, reasoning: 0 } };
const failure = { status: "execution_failed" as const, summary: "Python syntax error",
  failure: { cause: "program", recovery: "repair_program", code: "program_syntax",
    diagnostic: "Python syntax error", node: null, occurrence_path: [], catalog_digest: null,
    effects: [], dispatches: [], effects_unresolved: false } };
let requests = 0;
let first: { prompt: unknown[]; tools: unknown } | undefined;
const model = new MockLanguageModelV3({ doStream: async options => {
  const step = requests++;
  if (step === 0) first = { prompt: structuredClone(options.prompt), tools: structuredClone(options.tools) };
  if (step === 1) {
    assert.deepEqual(options.prompt.slice(0, first!.prompt.length), first!.prompt);
    assert.deepEqual(options.tools, first!.tools, "a syntax error does not restrict other tools");
    assert.match(JSON.stringify(options.prompt), /Python syntax error/);
    assert.doesNotMatch(JSON.stringify(options.prompt), /Host: repair the rejected call/);
  }
  const calls = [
    { toolCallId: "bad", toolName: "plasm", input: '{"program":"bad"}' },
    { toolCallId: "inspect", toolName: "inspect", input: '{}' },
    { toolCallId: "done", toolName: "complete_task", input: '{}' },
  ];
  return { stream: new ReadableStream({ start(controller) {
    controller.enqueue({ type: "tool-call", ...calls[step]! });
    controller.enqueue({ type: "finish", finishReason: { unified: "tool-calls", raw: undefined }, usage });
    controller.close();
  } }) };
} });
let inspected = 0;
const result = await runEveToolLoop({ model, system: "system", messages: [{ role: "user", content: "inspect" }],
  maxSteps: 5, agentName: "tool-errors", telemetry: { isEnabled: false }, tools: {
    plasm: tool({ inputSchema: toolInput(z.object({ program: z.string() })), execute: async () => failure }),
    inspect: tool({ inputSchema: toolInput(z.object({})), execute: async () => { inspected++; return "observed"; } }),
    ...createEvalTerminalTools({ sealEffectWorkflows: async () => {} }),
  } });
assert.equal(inspected, 1);
assert.equal(result.stopReason, "completed");
console.log("PASS: syntax errors retain ordinary history and full tool availability");
