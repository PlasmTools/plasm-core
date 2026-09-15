#!/usr/bin/env node
import assert from "node:assert/strict";
import { jsonSchema, tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import {
  evalTerminalGrade,
  formatPlasmDryRunMarkdown,
  formatPlasmRunMarkdown,
  hostMayInjectAppWorldComplete,
} from "../src/tools/format.js";
import {
  applyRunRefLedger,
  runEveToolLoop,
  runRefsInText,
} from "../src/telemetry/eve-tool-loop.js";
import { toolInput } from "../src/tools/tool-input.js";
import type { ModelMessage } from "ai";
import { z } from "zod";

const dry = formatPlasmDryRunMarkdown("plan review · 1w", "pc0");
assert.deepEqual(runRefsInText(dry), ["pc0"]);
assert.deepEqual(runRefsInText("clean read rows"), []);

const outstanding = new Set<string>();
applyRunRefLedger(outstanding, [
  { role: "tool", content: [{ type: "tool-result", toolName: "plasm", output: { type: "text", value: dry } }] } as ModelMessage,
]);
assert.deepEqual([...outstanding], ["pc0"]);
applyRunRefLedger(outstanding, [
  {
    role: "assistant",
    content: [{ type: "tool-call", toolCallId: "run-pc0", toolName: "plasm_run", input: { run_ref: "pc0" } }],
  } as ModelMessage,
  {
    role: "tool",
    content: [{ type: "tool-result", toolCallId: "run-pc0", toolName: "plasm_run", output: { type: "text", value: formatPlasmRunMarkdown("ok", true) } }],
  } as ModelMessage,
]);
assert.equal(outstanding.size, 0);

const twoPending = new Set<string>(["pc0", "pc1"]);
applyRunRefLedger(twoPending, [
  {
    role: "assistant",
    content: [{ type: "tool-call", toolCallId: "run-pc0", toolName: "plasm_run", input: { run_ref: "pc0" } }],
  } as ModelMessage,
  {
    role: "tool",
    content: [{ type: "tool-result", toolCallId: "run-pc0", toolName: "plasm_run", output: { type: "text", value: formatPlasmRunMarkdown("ok", true) } }],
  } as ModelMessage,
]);
assert.deepEqual([...twoPending], ["pc1"], "sibling pending handle must survive one successful run");

const continued = new Set<string>(["pc0"]);
applyRunRefLedger(continued, [
  {
    role: "assistant",
    content: [{ type: "tool-call", toolCallId: "run-pc0", toolName: "plasm_run", input: { run_ref: "pc0" } }],
  } as ModelMessage,
  {
    role: "tool",
    content: [{
      type: "tool-result",
      toolCallId: "run-pc0",
      toolName: "plasm_run",
      output: { type: "text", value: `${formatPlasmRunMarkdown("page", true)}\n\npass \`run_ref\`: \`pc2\` to **\`plasm_run\`**.` },
    }],
  } as ModelMessage,
]);
assert.deepEqual([...continued], ["pc2"], "continuation handle from plasm_run must stay pending");

let count = 0;
const model = new MockLanguageModelV3({
  doStream: async () => {
    const index = count++;
    const toolName = index === 0 ? "plasm" : index === 2 ? "plasm_run" : "complete_task";
    const isTool = index === 0 || index === 2 || index === 3;
    return {
      stream: new ReadableStream({
        start(controller) {
          if (isTool) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: `call-${index}`,
              toolName,
              input: toolName === "plasm_run" ? '{"run_ref":"pc0"}' : "{}",
            });
          } else {
            controller.enqueue({ type: "text-start", id: "text" });
            controller.enqueue({ type: "text-delta", id: "text", delta: "done without run" });
            controller.enqueue({ type: "text-end", id: "text" });
          }
          controller.enqueue({
            type: "finish",
            finishReason: { unified: isTool ? "tool-calls" : "stop", raw: undefined },
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

const tools = {
  plasm: tool({
    inputSchema: jsonSchema({ type: "object", properties: {} }),
    execute: async () => dry,
  }),
  plasm_run: tool({
    inputSchema: jsonSchema({ type: "object", properties: {} }),
    execute: async () => formatPlasmRunMarkdown("committed", true),
  }),
  complete_task: tool({
    inputSchema: jsonSchema({ type: "object", properties: {} }),
    execute: async () => "Task marked complete.",
  }),
};

const result = await runEveToolLoop({
  model,
  system: "system",
  tools,
  messages: [{ role: "user", content: "commit the reviewed write" }],
  maxSteps: 6,
  agentName: "test",
  telemetry: { isEnabled: false },
});

assert.equal(result.stopReason, "completed");
assert.ok(count >= 3, `loop must continue past unused run_ref stop, steps=${count}`);
const choice = model.doStreamCalls[2]?.toolChoice;
assert.ok(
  choice !== undefined &&
    typeof choice === "object" &&
    choice.type === "required",
  `step after unused run_ref must require a tool, got ${JSON.stringify(choice)}`,
);

let leftoverCount = 0;
const leftoverModel = new MockLanguageModelV3({
  doStream: async () => {
    const index = leftoverCount++;
    const isTool = index === 0;
    return {
      stream: new ReadableStream({
        start(controller) {
          if (isTool) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "mint-two",
              toolName: "plasm",
              input: "{}",
            });
          } else {
            controller.enqueue({ type: "text-start", id: "text" });
            controller.enqueue({ type: "text-delta", id: "text", delta: "stopping with work pending" });
            controller.enqueue({ type: "text-end", id: "text" });
          }
          controller.enqueue({
            type: "finish",
            finishReason: { unified: isTool ? "tool-calls" : "stop", raw: undefined },
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

const leftover = await runEveToolLoop({
  model: leftoverModel,
  system: "system",
  tools: {
    plasm: tool({
      inputSchema: jsonSchema({ type: "object", properties: {} }),
      execute: async () =>
        `${formatPlasmDryRunMarkdown("plan a", "pc0")}\n${formatPlasmDryRunMarkdown("plan b", "pc1")}`,
    }),
  },
  messages: [{ role: "user", content: "two writes" }],
  maxSteps: 2,
  agentName: "test-pending-final",
  telemetry: { isEnabled: false },
});
assert.equal(
  leftover.stopReason,
  "budget_exhausted",
  "final stop with leftover handles is unfinished, not completed",
);

let completeCount = 0;
const completeModel = new MockLanguageModelV3({
  doStream: async () => {
    completeCount += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({
            type: "tool-call",
            toolCallId: "done-1",
            toolName: "complete_task",
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

const completed = await runEveToolLoop({
  model: completeModel,
  system: "system",
  tools: {
    complete_task: tool({
      inputSchema: jsonSchema({ type: "object", properties: {} }),
      execute: async () => "Task marked complete with no answer.",
    }),
  },
  messages: [{ role: "user", content: "side effect done" }],
  maxSteps: 6,
  agentName: "test-complete-task",
  telemetry: { isEnabled: false },
});
assert.equal(completed.stopReason, "completed");
assert.equal(completeCount, 1, "complete_task must terminate the loop");

let submitCount = 0;
const submitModel = new MockLanguageModelV3({
  doStream: async () => {
    submitCount += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({
            type: "tool-call",
            toolCallId: "ans-1",
            toolName: "submit_answer",
            input: '{"answer":"classical"}',
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

const submitted = await runEveToolLoop({
  model: submitModel,
  system: "system",
  tools: {
    submit_answer: tool({
      inputSchema: jsonSchema({
        type: "object",
        properties: { answer: { type: "string" } },
        required: ["answer"],
      }),
      execute: async () => "Answer submitted.",
    }),
  },
  messages: [{ role: "user", content: "report the value" }],
  maxSteps: 6,
  agentName: "test-submit-answer",
  telemetry: { isEnabled: false },
});
assert.equal(submitted.stopReason, "completed");
assert.equal(submitCount, 1, "submit_answer must terminate the loop");

let chatStopCount = 0;
const chatStopModel = new MockLanguageModelV3({
  doStream: async () => {
    chatStopCount += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({ type: "text-start", id: "text" });
          controller.enqueue({ type: "text-delta", id: "text", delta: "leftover chat" });
          controller.enqueue({ type: "text-end", id: "text" });
          controller.enqueue({
            type: "finish",
            finishReason: { unified: "stop", raw: undefined },
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
const chatStop = await runEveToolLoop({
  model: chatStopModel,
  system: "system",
  tools: {
    complete_task: tool({
      inputSchema: jsonSchema({ type: "object", properties: {} }),
      execute: async () => "Task marked complete.",
    }),
  },
  messages: [{ role: "user", content: "just talk" }],
  maxSteps: 4,
  agentName: "test-chat-stop",
  telemetry: { isEnabled: false },
});
assert.equal(
  chatStop.stopReason,
  "unterminated",
  "mid-budget finishReason===stop without a terminal is unterminated, not budget_exhausted",
);

let invalidSubmitCount = 0;
const invalidSubmitModel = new MockLanguageModelV3({
  doStream: async () => {
    invalidSubmitCount += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({
            type: "tool-call",
            toolCallId: `bad-${invalidSubmitCount}`,
            toolName: "submit_answer",
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
const invalidSubmit = await runEveToolLoop({
  model: invalidSubmitModel,
  system: "system",
  tools: {
    submit_answer: tool({
      inputSchema: toolInput(z.object({ answer: z.string().min(1) })),
      execute: async (): Promise<string> => {
        throw new Error("submit_answer execute must not run on empty payload");
      },
    }),
  },
  messages: [{ role: "user", content: "report nothing" }],
  maxSteps: 3,
  agentName: "test-invalid-submit",
  telemetry: { isEnabled: false },
});
assert.notEqual(
  invalidSubmit.stopReason,
  "completed",
  "invalid submit_answer({}) must not terminate the grade path",
);
assert.ok(invalidSubmitCount >= 2, "invalid submit must not end the loop on first call");

let thrownCount = 0;
const thrownModel = new MockLanguageModelV3({
  doStream: async () => {
    thrownCount += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({
            type: "tool-call",
            toolCallId: `boom-${thrownCount}`,
            toolName: "complete_task",
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
const thrown = await runEveToolLoop({
  model: thrownModel,
  system: "system",
  tools: {
    complete_task: tool({
      inputSchema: toolInput(z.object({})),
      execute: async (): Promise<string> => {
        throw new Error("terminal execute failed");
      },
    }),
  },
  messages: [{ role: "user", content: "done" }],
  maxSteps: 3,
  agentName: "test-thrown-terminal",
  telemetry: { isEnabled: false },
});
assert.notEqual(
  thrown.stopReason,
  "completed",
  "thrown terminal execute must not count as validated termination",
);
assert.equal(
  hostMayInjectAppWorldComplete(
    evalTerminalGrade({ messages: thrown.messages, stopReason: thrown.stopReason }),
  ),
  false,
  "thrown complete_task must not invent AppWorld success+empty complete",
);
assert.equal(
  hostMayInjectAppWorldComplete(
    evalTerminalGrade({
      messages: invalidSubmit.messages,
      stopReason: invalidSubmit.stopReason,
    }),
  ),
  false,
  "invalid submit_answer must not invent AppWorld success+empty complete",
);

console.log("run-ref-not-terminal: ok");

