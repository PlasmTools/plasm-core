#!/usr/bin/env node
/**
 * Truncated / invalid tool JSON must not terminate the Eve loop mid-budget.
 * Host surfaces InvalidToolInputError, appends a repair diagnostic, continues
 * within maxSteps. Does not invent truncated fields or call complete_task.
 */
import assert from "node:assert/strict";
import { tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import {
  INVALID_TOOL_INPUT_REPAIR_DIAGNOSTIC,
  runEveToolLoop,
  stepHasInvalidToolInput,
} from "../src/telemetry/eve-tool-loop.js";
import { evalTerminalGrade } from "../src/tools/format.js";
import { createEvalTerminalTools } from "../src/tools/harness-tools.js";
import {
  TASK_LEDGER_TOOL_NAME,
  TaskLedgerStore,
  createTaskLedgerTool,
  emptyTaskLedger,
} from "../src/tools/task-ledger.js";
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
  stepHasInvalidToolInput([
    {
      role: "tool",
      content: [
        {
          type: "tool-result",
          toolCallId: "x",
          toolName: "task_ledger",
          output: {
            type: "error-text",
            value:
              'AI_InvalidToolInputError: Invalid input for tool task_ledger: AI_JSONParseError: JSON parsing failed: Text: {"action": "write", "record": .',
          },
        },
      ],
    },
  ]),
  true,
  "detector recognizes InvalidToolInputError error-text",
);
assert.equal(
  stepHasInvalidToolInput([
    {
      role: "tool",
      content: [
        {
          type: "tool-result",
          toolCallId: "x",
          toolName: "plasm",
          output: { type: "text", value: "plan ok" },
        },
      ],
    },
  ]),
  false,
  "ordinary tool success is not invalid input",
);
assert.equal(
  stepHasInvalidToolInput([
    {
      role: "tool",
      content: [
        {
          type: "tool-result",
          toolCallId: "x",
          toolName: "plasm",
          output: { type: "error-text", value: "**plasm** error: vendor 500" },
        },
      ],
    },
  ]),
  false,
  "ordinary execute error-text is not InvalidToolInput",
);

const validLedger = {
  action: "write" as const,
  record: {
    ...emptyTaskLedger(),
    requested_outcomes: ["settle matching debts"],
    remaining_work: ["complete_task"],
  },
};

let callCount = 0;
const truncatedThenRepairModel = new MockLanguageModelV3({
  doStream: async () => {
    const index = callCount++;
    return {
      stream: new ReadableStream({
        start(controller) {
          if (index === 0) {
            // Witness shape: truncated task_ledger JSON + finishReason length.
            controller.enqueue({
              type: "tool-call",
              toolCallId: "ledger-trunc",
              toolName: TASK_LEDGER_TOOL_NAME,
              input: '{"action": "write", "record":',
            });
            controller.enqueue(finish("length"));
          } else if (index === 1) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "ledger-ok",
              toolName: TASK_LEDGER_TOOL_NAME,
              input: JSON.stringify(validLedger),
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

const store = new TaskLedgerStore();
const tools = {
  ...createTaskLedgerTool(store),
  ...createEvalTerminalTools(),
};

const repaired = await runEveToolLoop({
  model: truncatedThenRepairModel,
  system: "system",
  tools,
  messages: [{ role: "user", content: "record ledger then finish" }],
  maxSteps: 8,
  agentName: "test-invalid-tool-input-retry",
  telemetry: { isEnabled: false },
});

assert.ok(callCount >= 3, `loop must continue after truncated JSON (calls=${callCount})`);
assert.equal(repaired.stopReason, "completed", "successful complete_task after repair");
assert.deepEqual(
  repaired.stepFinishReasons.slice(0, 3),
  ["length", "tool-calls", "tool-calls"],
  "per-step finishReason must be preserved (length vs tool-calls)",
);
assert.ok(
  repaired.messages.some(
    (m) =>
      m.role === "user" &&
      typeof m.content === "string" &&
      m.content.includes(INVALID_TOOL_INPUT_REPAIR_DIAGNOSTIC),
  ),
  "repair diagnostic must reach the model",
);
assert.ok(
  repaired.messages.some((m) => {
    if (m.role !== "tool" || !Array.isArray(m.content)) return false;
    return m.content.some((part) => {
      if (!part || typeof part !== "object") return false;
      const out = (part as { output?: { type?: string; value?: string } }).output;
      return (
        out?.type === "error-text" &&
        typeof out.value === "string" &&
        out.value.includes("InvalidToolInput")
      );
    });
  }),
  "validation error must be returned to the model",
);
assert.deepEqual(store.record.requested_outcomes, ["settle matching debts"]);
assert.equal(
  evalTerminalGrade({ messages: repaired.messages, stopReason: repaired.stopReason }).kind,
  "null",
);

// Truncated input alone must not freeze the aggregate as unterminated while budget remains.
let midBudgetCalls = 0;
const truncatedThenProseModel = new MockLanguageModelV3({
  doStream: async () => {
    const index = midBudgetCalls++;
    return {
      stream: new ReadableStream({
        start(controller) {
          if (index === 0) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "bad-any",
              toolName: "echo_box",
              input: '{"note":',
            });
            controller.enqueue(finish("length"));
          } else {
            controller.enqueue({ type: "text-start", id: "t" });
            controller.enqueue({ type: "text-delta", id: "t", delta: "gave up in prose" });
            controller.enqueue({ type: "text-end", id: "t" });
            controller.enqueue(finish("stop"));
          }
          controller.close();
        },
      }),
    };
  },
});

const midBudget = await runEveToolLoop({
  model: truncatedThenProseModel,
  system: "system",
  tools: {
    echo_box: tool({
      description: "echo",
      inputSchema: toolInput(z.object({ note: z.string().min(1) })),
      execute: async ({ note }) => note,
    }),
    ...createEvalTerminalTools(),
  },
  messages: [{ role: "user", content: "try a tool" }],
  maxSteps: 6,
  agentName: "test-invalid-then-prose",
  telemetry: { isEnabled: false },
});

assert.ok(midBudgetCalls >= 2, "truncated step must not end the loop alone");
assert.equal(
  midBudget.stopReason,
  "unterminated",
  "later mid-budget prose stop is unterminated — truncated input did not fake-complete",
);
assert.equal(
  evalTerminalGrade({ messages: midBudget.messages, stopReason: midBudget.stopReason }).kind,
  "unterminated",
);
assert.deepEqual(midBudget.stepFinishReasons, ["length", "stop"]);

console.log("ok: invalid-tool-input-retry");
