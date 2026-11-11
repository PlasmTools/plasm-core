#!/usr/bin/env node
/**
 * Truncated / invalid tool JSON must not terminate the Eve loop mid-budget.
 * The normal tool error stays in append-only history, with the full tool set
 * available. No incomplete call is executed or silently replayed.
 */
import assert from "node:assert/strict";
import { tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import {
  runEveToolLoop,
} from "../src/telemetry/eve-tool-loop.js";
import { evalTerminalGrade } from "../src/tools/format.js";
import { createEvalTerminalTools } from "../src/tools/harness-tools.js";
import { toolInput } from "../src/tools/tool-input.js";
import { z } from "zod";

const usage = {
  inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
  outputTokens: { total: 2, text: 2, reasoning: 0 },
};

function finish(reason: "tool-calls" | "length" | "stop" | "other") {
  return {
    type: "finish" as const,
    finishReason: { unified: reason, raw: undefined },
    usage,
  };
}

const validRecord = {
  action: "write" as const,
  record: { value: "updated" },
};
let stored = "";
const fixtureTool = tool({
  inputSchema: toolInput(z.object({ action: z.enum(["read", "write"]), record: z.object({ value: z.string() }).optional() })),
  execute: async (input) => { if (input.action === "write") stored = input.record?.value ?? ""; return stored; },
});

let callCount = 0;
let firstRequest: { prompt: unknown[]; tools: unknown } | undefined;
const truncatedThenRepairModel = new MockLanguageModelV3({
  doStream: async (options) => {
    const index = callCount++;
    if (index === 0) firstRequest = { prompt: structuredClone(options.prompt), tools: structuredClone(options.tools) };
    if (index === 1) {
      assert.deepEqual(options.prompt.slice(0, firstRequest!.prompt.length), firstRequest!.prompt,
        "the full earlier prompt remains a stable prefix");
      assert.deepEqual(options.tools, firstRequest!.tools, "malformed calls do not hide other tools");
      const previousCall = options.prompt
        .filter(message => message.role === "assistant")
        .flatMap(message => message.content)
        .find(part => part.type === "tool-call" && part.toolCallId === "record-trunc");
      assert.ok(previousCall, "rejected call remains in ordinary assistant history");
      assert(!JSON.stringify(options.prompt).includes("Host: repair the rejected call"));
      const validSibling = options.prompt
        .filter(message => message.role === "assistant")
        .flatMap(message => message.content)
        .find(part => part.type === "tool-call" && part.toolCallId === "record-read");
      assert.deepEqual(
        validSibling?.type === "tool-call" ? validSibling.input : undefined,
        { action: "read" },
        "a valid sibling call must retain its parsed input",
      );
    }
    return {
      stream: new ReadableStream({
        start(controller) {
          if (index === 0) {
            // Truncated JSON and a valid sibling with mapped finishReason other.
            controller.enqueue({
              type: "tool-call",
              toolCallId: "record-trunc",
              toolName: "fixture_record",
              input: '{"action": "write", "record":',
            });
            controller.enqueue({
              type: "tool-call",
              toolCallId: "record-read",
              toolName: "fixture_record",
              input: '{"action":"read"}',
            });
            controller.enqueue(finish("other"));
          } else if (index === 1) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "record-ok",
              toolName: "fixture_record",
              input: JSON.stringify(validRecord),
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

const tools = {
  fixture_record: fixtureTool,
  ...createEvalTerminalTools({ sealEffectWorkflows: async () => {} }),
};

const repaired = await runEveToolLoop({
  model: truncatedThenRepairModel,
  system: "system",
  tools,
  messages: [{ role: "user", content: "update record then finish" }],
  maxSteps: 8,
  agentName: "test-invalid-tool-input-retry",
  telemetry: { isEnabled: false },
});

assert.ok(callCount >= 3, `loop must continue after truncated JSON (calls=${callCount})`);
assert.equal(repaired.stopReason, "completed", "successful complete_task after repair");
assert.deepEqual(
  repaired.stepFinishReasons.slice(0, 3),
  ["other", "tool-calls", "tool-calls"],
  "per-step finishReason must be preserved (other vs tool-calls)",
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
assert.equal(stored, "updated");
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
    ...createEvalTerminalTools({ sealEffectWorkflows: async () => {} }),
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
  "ordinary prose does not satisfy the eval terminal",
);
assert.equal(
  evalTerminalGrade({ messages: midBudget.messages, stopReason: midBudget.stopReason }).kind,
  "unterminated",
);
assert.deepEqual(midBudget.stepFinishReasons, ["length", "stop"]);


let providerCalls = 0;
const providerFailureThenTerminal = new MockLanguageModelV3({
  doStream: async (options) => {
    const index = providerCalls++;
    if (index === 1) {
      const feedback = options.prompt.filter(m => m.role === "user").map(m => JSON.stringify(m.content)).join("\n");
      assert(feedback.includes("provider stream failed"), "provider error must not be described as a model JSON mistake");
      assert(!feedback.includes("repair attempt"), "do not append contradictory JSON repair feedback");
    }
    return {stream:new ReadableStream({start(controller) {
      if (index === 0) {
        controller.enqueue({type:"text-start",id:"provider-text"});
        controller.enqueue({type:"text-delta",id:"provider-text",delta:"working"});
        controller.enqueue({type:"text-end",id:"provider-text"});
        controller.enqueue({type:"tool-call",toolCallId:"provider-cut",toolName:"fixture_record",input:'{"action":"write","record":'});
        controller.enqueue({type:"error",error:new Error("Upstream provider unavailable (502)")});
        controller.enqueue(finish("other"));
      } else {
        controller.enqueue({type:"tool-call",toolCallId:"finish",toolName:"complete_task",input:"{}"});
        controller.enqueue(finish("tool-calls"));
      }
      controller.close();
    }})};
  }
});
const providerResult = await runEveToolLoop({model:providerFailureThenTerminal,system:"system",
  tools:{fixture_record: fixtureTool,...createEvalTerminalTools({ sealEffectWorkflows: async () => {} })},
  messages:[{role:"user",content:"perform task"}],maxSteps:3,agentName:"test-provider-error",telemetry:{isEnabled:false}});
assert.equal(providerResult.stopReason,"completed");
assert.deepEqual(providerResult.stepFinishReasons,["error","tool-calls"]);

console.log("ok: invalid-tool-input-retry (including provider stream errors)");
