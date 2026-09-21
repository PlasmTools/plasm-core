#!/usr/bin/env node
/** Eval terminals require an opened workflow; initialization is host-owned. */
import assert from "node:assert/strict";
import { tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import { z } from "zod";

import {
  createEvalTerminalTools,
  EVAL_TERMINAL_REQUIRES_DISCOVERY,
} from "../src/tools/harness-tools.js";
import {
  COMPLETE_TASK_TOOL_NAME,
  SUBMIT_ANSWER_TOOL_NAME,
  evalTerminalGrade,
} from "../src/tools/format.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";
import { toolInput } from "../src/tools/tool-input.js";
const INITIAL_DISCOVERY_TOOL_NAME = "plasm_context";

// --- Unit: refusal throws (tool-error path), success after discovery ---

let discovered = false;
const tools = createEvalTerminalTools({
  discoveryCompleted: () => discovered,
});

const complete = tools[COMPLETE_TASK_TOOL_NAME]?.execute;
const submit = tools[SUBMIT_ANSWER_TOOL_NAME]?.execute;
assert.ok(complete);
assert.ok(submit);
const executionOptions = { toolCallId: "unit", messages: [], context: {} };

await assert.rejects(
  async () => complete({}, executionOptions),
  (err: unknown) =>
    err instanceof Error && err.message === EVAL_TERMINAL_REQUIRES_DISCOVERY,
  "complete_task refusal must throw (tool-error), not return success text",
);

await assert.rejects(
  async () => submit({ answer: "42" }, executionOptions),
  (err: unknown) =>
    err instanceof Error && err.message === EVAL_TERMINAL_REQUIRES_DISCOVERY,
  "submit_answer refusal must throw (tool-error), not return success text",
);

discovered = true;
assert.equal(await complete({}, executionOptions), "Task marked complete.");
assert.equal(await submit({ answer: "42" }, executionOptions), "Answer submitted.");

const ungated = createEvalTerminalTools();
const ungatedComplete = ungated[COMPLETE_TASK_TOOL_NAME]?.execute;
assert.ok(ungatedComplete);
assert.equal(await ungatedComplete({}, executionOptions), "Task marked complete.");

// --- Loop P1-1: discovery false + complete_task → NOT completed / NOT null grade ---

let blockedCalls = 0;
const blockedModel = new MockLanguageModelV3({
  doStream: async () => {
    blockedCalls += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({
            type: "tool-call",
            toolCallId: "blocked-complete",
            toolName: COMPLETE_TASK_TOOL_NAME,
            input: "{}",
          });
          controller.enqueue({
            type: "finish",
            finishReason: { unified: "tool-calls", raw: undefined },
            usage: {
              inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
              outputTokens: { total: 2, text: 2, reasoning: 0 },
            },
          });
          controller.close();
        },
      }),
    };
  },
});

const blockedTerminals = createEvalTerminalTools({
  discoveryCompleted: () => false,
});

const blocked = await runEveToolLoop({
  model: blockedModel,
  system: "system",
  tools: {
    ...blockedTerminals,
    [INITIAL_DISCOVERY_TOOL_NAME]: tool({
      inputSchema: toolInput(z.object({ intent: z.string() })),
      execute: async () => "should not be required for this case",
    }),
  },
  messages: [{ role: "user", content: "done without discovery" }],
  maxSteps: 3,
  agentName: "test-discovery-gate-refusal",
  telemetry: { isEnabled: false },
  // Do not enforce first-step discovery here — isolate terminal refusal grading.
});

assert.notEqual(
  blocked.stopReason,
  "completed",
  "discovery-gate refusal must not yield stopReason completed",
);
assert.deepEqual(
  evalTerminalGrade({ messages: blocked.messages, stopReason: blocked.stopReason }),
  { kind: "unterminated" },
  "discovery-gate refusal must not grade as null/complete_task success",
);
assert.ok(blockedCalls >= 1, "model must have attempted complete_task");


console.log("PASS: terminal refusal remains an error until a workflow exists");
