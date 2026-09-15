#!/usr/bin/env node
/**
 * Generation-budget law:
 * - finishReason=length + empty text + remaining steps → continue (not unterminated kill)
 * - length + truncated tool JSON → InvalidToolInput repair path; never execute bad args
 * - length after successful tool execution → continue from preserved results (not "before a tool call")
 * - modelOptions.maxOutputTokens is passed through (configurable ceiling respected)
 * - length alone must not become completed / complete_task / submit_answer
 * Raising maxSteps alone does not enlarge per-generation output.
 */
import assert from "node:assert/strict";
import { tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import {
  GENERATION_TRUNCATED_AFTER_TOOLS_DIAGNOSTIC,
  GENERATION_TRUNCATED_CONTINUE_DIAGNOSTIC,
  INVALID_TOOL_INPUT_REPAIR_DIAGNOSTIC,
  runEveToolLoop,
  shouldContinueAfterGenerationLength,
} from "../src/telemetry/eve-tool-loop.js";
import { evalTerminalGrade } from "../src/tools/format.js";
import { createEvalTerminalTools } from "../src/tools/harness-tools.js";
import { toolInput } from "../src/tools/tool-input.js";
import { z } from "zod";

const usage = {
  inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
  outputTokens: { total: 2, text: 2, reasoning: 0 },
};

function finish(reason: "tool-calls" | "length" | "stop") {
  return {
    type: "finish" as const,
    finishReason: { unified: reason, raw: undefined },
    usage,
  };
}

assert.equal(
  shouldContinueAfterGenerationLength({
    finishReason: "length",
    stepsUsed: 1,
    maxSteps: 8,
    hasValidatedTerminal: false,
  }),
  true,
);
assert.equal(
  shouldContinueAfterGenerationLength({
    finishReason: "stop",
    stepsUsed: 1,
    maxSteps: 8,
    hasValidatedTerminal: false,
  }),
  false,
  "ordinary stop must not use length-continuation",
);
assert.equal(
  shouldContinueAfterGenerationLength({
    finishReason: "length",
    stepsUsed: 8,
    maxSteps: 8,
    hasValidatedTerminal: false,
  }),
  false,
  "length on last step must not invent extra budget",
);
assert.equal(
  shouldContinueAfterGenerationLength({
    finishReason: "length",
    stepsUsed: 2,
    maxSteps: 8,
    hasValidatedTerminal: true,
  }),
  false,
  "validated terminal must not be overwritten by length continue",
);

// --- length + empty text + remaining steps → continue, then terminal ---
let emptyLengthCalls = 0;
let echoExecuted = 0;
const emptyThenToolModel = new MockLanguageModelV3({
  doStream: async () => {
    const index = emptyLengthCalls++;
    return {
      stream: new ReadableStream({
        start(controller) {
          if (index === 0) {
            // Truncated thinking/prose: no tool call, finishReason length.
            controller.enqueue(finish("length"));
          } else if (index === 1) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "echo-1",
              toolName: "echo_box",
              input: JSON.stringify({ note: "resumed" }),
            });
            controller.enqueue(finish("tool-calls"));
          } else {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "done",
              toolName: "complete_task",
              input: "{}",
            });
            controller.enqueue(finish("tool-calls"));
          }
          controller.close();
        },
      }),
    };
  },
});

const emptyContinued = await runEveToolLoop({
  model: emptyThenToolModel,
  system: "system",
  tools: {
    echo_box: tool({
      description: "echo",
      inputSchema: toolInput(z.object({ note: z.string().min(1) })),
      execute: async ({ note }) => {
        echoExecuted += 1;
        return note;
      },
    }),
    ...createEvalTerminalTools(),
  },
  messages: [{ role: "user", content: "do the task" }],
  maxSteps: 8,
  agentName: "test-length-empty-continue",
  telemetry: { isEnabled: false },
  modelOptions: { maxOutputTokens: 12_288 },
});

assert.ok(emptyLengthCalls >= 3, `must continue after empty length (calls=${emptyLengthCalls})`);
assert.equal(echoExecuted, 1, "tool after length continue must execute once");
assert.equal(emptyContinued.stopReason, "completed");
assert.equal(emptyContinued.lengthTruncationCount, 1);
assert.equal(emptyContinued.maxOutputTokens, 12_288);
assert.deepEqual(emptyContinued.stepFinishReasons.slice(0, 3), [
  "length",
  "tool-calls",
  "tool-calls",
]);
assert.ok(
  emptyContinued.messages.some(
    (m) =>
      m.role === "user" &&
      typeof m.content === "string" &&
      m.content.includes(GENERATION_TRUNCATED_CONTINUE_DIAGNOSTIC),
  ),
  "generation-truncated diagnostic must reach the model",
);
assert.ok(
  emptyThenToolModel.doStreamCalls.every((c) => c.maxOutputTokens === 12_288),
  "configurable maxOutputTokens must be passed on every generation",
);

// --- length + truncated tool JSON → repair path; never execute truncated args ---
let truncCalls = 0;
let truncExecuted = 0;
const truncThenRepairModel = new MockLanguageModelV3({
  doStream: async () => {
    const index = truncCalls++;
    return {
      stream: new ReadableStream({
        start(controller) {
          if (index === 0) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "echo-trunc",
              toolName: "echo_box",
              input: '{"note":',
            });
            controller.enqueue(finish("length"));
          } else if (index === 1) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "echo-ok",
              toolName: "echo_box",
              input: JSON.stringify({ note: "repaired" }),
            });
            controller.enqueue(finish("tool-calls"));
          } else {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "done",
              toolName: "complete_task",
              input: "{}",
            });
            controller.enqueue(finish("tool-calls"));
          }
          controller.close();
        },
      }),
    };
  },
});

const repaired = await runEveToolLoop({
  model: truncThenRepairModel,
  system: "system",
  tools: {
    echo_box: tool({
      description: "echo",
      inputSchema: toolInput(z.object({ note: z.string().min(1) })),
      execute: async ({ note }) => {
        truncExecuted += 1;
        return note;
      },
    }),
    ...createEvalTerminalTools(),
  },
  messages: [{ role: "user", content: "try a tool" }],
  maxSteps: 8,
  agentName: "test-length-trunc-repair",
  telemetry: { isEnabled: false },
  modelOptions: { maxOutputTokens: 4096 },
});

assert.equal(truncExecuted, 1, "only the repaired call may execute");
assert.equal(repaired.stopReason, "completed");
assert.ok(
  repaired.messages.some(
    (m) =>
      m.role === "user" &&
      typeof m.content === "string" &&
      m.content.includes(INVALID_TOOL_INPUT_REPAIR_DIAGNOSTIC),
  ),
  "InvalidToolInput repair diagnostic required for truncated tool JSON",
);
assert.ok(
  !repaired.messages.some(
    (m) =>
      m.role === "user" &&
      typeof m.content === "string" &&
      m.content.includes(GENERATION_TRUNCATED_CONTINUE_DIAGNOSTIC),
  ),
  "do not double-stack generation-truncated diagnostic on InvalidToolInput path",
);
assert.equal(
  truncThenRepairModel.doStreamCalls[0]?.maxOutputTokens,
  4096,
  "configured ceiling must still be respected on truncated-tool generations",
);

// --- length after successful tool execution → preserve results; after-tools diagnostic ---
let mixedCalls = 0;
let mixedExecuted = 0;
const mixedLengthModel = new MockLanguageModelV3({
  doStream: async () => {
    const index = mixedCalls++;
    return {
      stream: new ReadableStream({
        start(controller) {
          if (index === 0) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "echo-ok",
              toolName: "echo_box",
              input: JSON.stringify({ note: "side-effect-done" }),
            });
            // Valid tool executed, then generation hit the output ceiling.
            controller.enqueue(finish("length"));
          } else {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "done",
              toolName: "complete_task",
              input: "{}",
            });
            controller.enqueue(finish("tool-calls"));
          }
          controller.close();
        },
      }),
    };
  },
});

const mixedContinued = await runEveToolLoop({
  model: mixedLengthModel,
  system: "system",
  tools: {
    echo_box: tool({
      description: "echo",
      inputSchema: toolInput(z.object({ note: z.string().min(1) })),
      execute: async ({ note }) => {
        mixedExecuted += 1;
        return note;
      },
    }),
    ...createEvalTerminalTools(),
  },
  messages: [{ role: "user", content: "run then continue" }],
  maxSteps: 8,
  agentName: "test-length-after-tools",
  telemetry: { isEnabled: false },
  modelOptions: { maxOutputTokens: 8192 },
});

assert.equal(mixedExecuted, 1, "successful tool must execute once before length continue");
assert.equal(mixedContinued.stopReason, "completed");
assert.equal(mixedContinued.lengthTruncationCount, 1);
assert.ok(
  mixedContinued.messages.some(
    (m) =>
      m.role === "user" &&
      typeof m.content === "string" &&
      m.content.includes(GENERATION_TRUNCATED_AFTER_TOOLS_DIAGNOSTIC),
  ),
  "length after successful tools must use after-tools diagnostic",
);
assert.ok(
  !mixedContinued.messages.some(
    (m) =>
      m.role === "user" &&
      typeof m.content === "string" &&
      m.content.includes(GENERATION_TRUNCATED_CONTINUE_DIAGNOSTIC),
  ),
  "must not claim budget exhausted before a complete tool call when a tool succeeded",
);
assert.ok(
  !mixedContinued.messages.some(
    (m) =>
      m.role === "user" &&
      typeof m.content === "string" &&
      m.content.includes(INVALID_TOOL_INPUT_REPAIR_DIAGNOSTIC),
  ),
  "successful tool + length must not stack InvalidToolInput repair",
);
assert.ok(
  mixedContinued.messages.some((m) => {
    if (m.role !== "tool" || !Array.isArray(m.content)) return false;
    return m.content.some(
      (p) =>
        p &&
        typeof p === "object" &&
        (p as { type?: string; toolName?: string }).type === "tool-result" &&
        (p as { toolName?: string }).toolName === "echo_box",
    );
  }),
  "successful tool results must remain in the transcript for continuation",
);

// --- length alone must not become success / complete_task ---
let lengthOnlyCalls = 0;
const lengthExhaustModel = new MockLanguageModelV3({
  doStream: async () => {
    lengthOnlyCalls += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue(finish("length"));
          controller.close();
        },
      }),
    };
  },
});

const lengthOnly = await runEveToolLoop({
  model: lengthExhaustModel,
  system: "system",
  tools: createEvalTerminalTools(),
  messages: [{ role: "user", content: "never finish" }],
  maxSteps: 3,
  agentName: "test-length-not-complete",
  telemetry: { isEnabled: false },
  modelOptions: { maxOutputTokens: 2048 },
});

assert.equal(lengthOnlyCalls, 3, "length continues until step budget");
assert.equal(lengthOnly.stopReason, "budget_exhausted");
assert.notEqual(lengthOnly.stopReason, "completed");
assert.equal(lengthOnly.lengthTruncationCount, 3);
assert.equal(
  evalTerminalGrade({ messages: lengthOnly.messages, stopReason: lengthOnly.stopReason }).kind,
  "unterminated",
  "length must not invent complete_task / submit_answer",
);
assert.ok(
  !lengthOnly.messages.some((m) => {
    if (m.role !== "assistant" || !Array.isArray(m.content)) return false;
    return m.content.some(
      (p) =>
        p &&
        typeof p === "object" &&
        (p as { toolName?: string }).toolName === "complete_task",
    );
  }),
  "host must not emit complete_task for length",
);

console.log("ok: generation-length-continue");
