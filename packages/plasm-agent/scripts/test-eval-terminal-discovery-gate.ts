#!/usr/bin/env node
/**
 * Structural lifecycle gate: eval terminals require initial plasm_context.
 *
 * P1-1: Refusal is tool-error — not ordinary success text — so the loop cannot
 * grade a blocked complete_task as completed / null.
 * P1-2: Prose-only stop before discovery is forced discovery (continues with
 * plasm_context until valid response or budget), not a mid-budget unterminated free pass.
 * P1-3: Auto-choice compatibility leaves provider choice unset while retaining
 * the mandatory discovery lifecycle gate.
 * P1-4: Malformed plasm_context (schema validation / non-execution) must not
 * satisfy discovery — prose after the bad call must not get a free unterminated
 * mid-budget exit. Valid insufficient response clears the force (clarification OK).
 */
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
import {
  INITIAL_DISCOVERY_TOOL_NAME,
  runEveToolLoop,
  stepHasValidDiscoveryResponse,
} from "../src/telemetry/eve-tool-loop.js";
import { toolInput } from "../src/tools/tool-input.js";

function isForcedDiscoveryChoice(choice: unknown): boolean {
  return (
    choice !== undefined &&
    typeof choice === "object" &&
    choice !== null &&
    (choice as { type?: string; toolName?: string }).type === "tool" &&
    (choice as { toolName?: string }).toolName === INITIAL_DISCOVERY_TOOL_NAME
  );
}

// --- Unit: satisfaction predicate — name alone / tool-error do not count ---

assert.equal(
  stepHasValidDiscoveryResponse([], [
    {
      type: "tool-error",
      toolName: INITIAL_DISCOVERY_TOOL_NAME,
      error: "AI_InvalidToolInputError: Invalid input for tool plasm_context",
    },
  ]),
  false,
  "validation tool-error must not satisfy discovery",
);
assert.equal(
  stepHasValidDiscoveryResponse([], [
    {
      type: "tool-result",
      toolName: INITIAL_DISCOVERY_TOOL_NAME,
      output: {
        type: "text",
        value: "**plasm_context:** insufficient\nNo supporting capabilities.",
      },
    },
  ]),
  true,
  "successful insufficient tool-result must satisfy discovery",
);

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
  requireInitialDiscovery: false,
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

// --- Loop P1-2: prose-only before discovery → forced discovery, not free unterminated ---

let proseCalls = 0;
const proseChoices: unknown[] = [];
const proseModel = new MockLanguageModelV3({
  doStream: async (opts) => {
    proseCalls += 1;
    proseChoices.push(opts.toolChoice);
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({ type: "text-start", id: "text" });
          controller.enqueue({
            type: "text-delta",
            id: "text",
            delta: "No access — abandoning without discovery.",
          });
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

const proseBeforeDiscovery = await runEveToolLoop({
  model: proseModel,
  system: "system",
  tools: {
    [INITIAL_DISCOVERY_TOOL_NAME]: tool({
      inputSchema: toolInput(z.object({ intent: z.string() })),
      execute: async () => "discovery markdown",
    }),
    ...createEvalTerminalTools({ discoveryCompleted: () => false }),
  },
  messages: [{ role: "user", content: "abandon via prose" }],
  maxSteps: 3,
  agentName: "test-prose-before-discovery",
  telemetry: { isEnabled: false },
  requireInitialDiscovery: true,
  discoveryCompleted: () => false,
});

assert.equal(
  proseBeforeDiscovery.stopReason,
  "budget_exhausted",
  "prose-only before discovery is forced discovery until budget — not mid-budget unterminated",
);
assert.equal(proseCalls, 3, "loop must refuse prose exit until discovery attempt or budget");
assert.ok(
  proseChoices.every(isForcedDiscoveryChoice),
  "every step before discovery attempt must force plasm_context toolChoice",
);
assert.deepEqual(
  evalTerminalGrade({
    messages: proseBeforeDiscovery.messages,
    stopReason: proseBeforeDiscovery.stopReason,
  }),
  { kind: "unterminated" },
);

// --- Loop P1-3: auto-choice compatibility retains the lifecycle gate ---

let autoChoiceCalls = 0;
const autoChoices: unknown[] = [];
const autoChoiceModel = new MockLanguageModelV3({
  doStream: async (opts) => {
    autoChoiceCalls += 1;
    autoChoices.push(opts.toolChoice);
    return {
      stream: new ReadableStream({
        start(controller) {
          controller.enqueue({ type: "text-start", id: "text" });
          controller.enqueue({
            type: "text-delta",
            id: "text",
            delta: "Still no discovery.",
          });
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

const autoChoiceBeforeDiscovery = await runEveToolLoop({
  model: autoChoiceModel,
  system: "system",
  tools: {
    [INITIAL_DISCOVERY_TOOL_NAME]: tool({
      inputSchema: toolInput(z.object({ intent: z.string() })),
      execute: async () => "discovery markdown",
    }),
    ...createEvalTerminalTools({ discoveryCompleted: () => false }),
  },
  messages: [{ role: "user", content: "use auto choice" }],
  maxSteps: 2,
  agentName: "test-auto-choice-before-discovery",
  telemetry: { isEnabled: false },
  requireInitialDiscovery: true,
  forceToolChoice: false,
  discoveryCompleted: () => false,
});

assert.equal(autoChoiceCalls, 2, "lifecycle gate must continue until budget");
assert.ok(
  autoChoices.every((choice) => !isForcedDiscoveryChoice(choice)),
  "provider tool choice must remain auto while discovery stays mandatory",
);
assert.equal(autoChoiceBeforeDiscovery.stopReason, "budget_exhausted");

// --- Loop P1-4: malformed plasm_context → discovery still forced; prose must not free-exit ---

let malformedCalls = 0;
let discoveryExecutions = 0;
const malformedChoices: unknown[] = [];
const malformedModel = new MockLanguageModelV3({
  doStream: async (opts) => {
    const index = malformedCalls++;
    malformedChoices.push(opts.toolChoice);
    return {
      stream: new ReadableStream({
        start(controller) {
          if (index === 0) {
            // Fabricator reproduction: plasm_context({}) — schema validation failure.
            controller.enqueue({
              type: "tool-call",
              toolCallId: "ctx-malformed",
              toolName: INITIAL_DISCOVERY_TOOL_NAME,
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
          } else {
            // Prose after malformed call — must NOT clear force / free unterminate.
            controller.enqueue({ type: "text-start", id: "text" });
            controller.enqueue({
              type: "text-delta",
              id: "text",
              delta: "Giving up after bad plasm_context args.",
            });
            controller.enqueue({ type: "text-end", id: "text" });
            controller.enqueue({
              type: "finish",
              finishReason: { unified: "stop", raw: undefined },
              usage: {
                inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
                outputTokens: { total: 2, text: 2, reasoning: 0 },
              },
            });
          }
          controller.close();
        },
      }),
    };
  },
});

const malformedDiscovery = await runEveToolLoop({
  model: malformedModel,
  system: "system",
  tools: {
    [INITIAL_DISCOVERY_TOOL_NAME]: tool({
      inputSchema: toolInput(z.object({ intent: z.string() })),
      execute: async () => {
        discoveryExecutions += 1;
        return "must not execute on malformed args";
      },
    }),
    ...createEvalTerminalTools({ discoveryCompleted: () => false }),
  },
  messages: [{ role: "user", content: "malformed context then prose" }],
  maxSteps: 3,
  agentName: "test-malformed-discovery-not-satisfied",
  telemetry: { isEnabled: false },
  requireInitialDiscovery: true,
  discoveryCompleted: () => false,
});

assert.equal(
  discoveryExecutions,
  0,
  "malformed plasm_context must not execute discovery",
);
assert.equal(
  malformedDiscovery.stopReason,
  "budget_exhausted",
  "prose after malformed plasm_context must not mid-budget unterminate as if discovery happened",
);
assert.equal(
  malformedCalls,
  3,
  "loop must keep forcing discovery after validation failure until budget",
);
assert.ok(
  malformedChoices.every(isForcedDiscoveryChoice),
  "toolChoice must stay forced to plasm_context after malformed call (not auto)",
);
assert.deepEqual(
  evalTerminalGrade({
    messages: malformedDiscovery.messages,
    stopReason: malformedDiscovery.stopReason,
  }),
  { kind: "unterminated" },
);

// After valid (including insufficient) plasm_context response, prose clarification may stop.
let afterCalls = 0;
const afterChoices: unknown[] = [];
const afterModel = new MockLanguageModelV3({
  doStream: async (opts) => {
    const index = afterCalls++;
    afterChoices.push(opts.toolChoice);
    return {
      stream: new ReadableStream({
        start(controller) {
          if (index === 0) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "ctx-0",
              toolName: INITIAL_DISCOVERY_TOOL_NAME,
              input: '{"intent":"task"}',
            });
            controller.enqueue({
              type: "finish",
              finishReason: { unified: "tool-calls", raw: undefined },
              usage: {
                inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
                outputTokens: { total: 2, text: 2, reasoning: 0 },
              },
            });
          } else {
            controller.enqueue({ type: "text-start", id: "text" });
            controller.enqueue({
              type: "text-delta",
              id: "text",
              delta: "Need clarification after investigation.",
            });
            controller.enqueue({ type: "text-end", id: "text" });
            controller.enqueue({
              type: "finish",
              finishReason: { unified: "stop", raw: undefined },
              usage: {
                inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
                outputTokens: { total: 2, text: 2, reasoning: 0 },
              },
            });
          }
          controller.close();
        },
      }),
    };
  },
});

const afterInvestigation = await runEveToolLoop({
  model: afterModel,
  system: "system",
  tools: {
    [INITIAL_DISCOVERY_TOOL_NAME]: tool({
      inputSchema: toolInput(z.object({ intent: z.string() })),
      execute: async () =>
        "**plasm_context:** insufficient\nNo supporting capabilities in the presented universe.",
    }),
    ...createEvalTerminalTools({ discoveryCompleted: () => false }),
  },
  messages: [{ role: "user", content: "clarify after discovery" }],
  maxSteps: 5,
  agentName: "test-clarify-after-discovery",
  telemetry: { isEnabled: false },
  requireInitialDiscovery: true,
  discoveryCompleted: () => false,
});

assert.equal(
  afterInvestigation.stopReason,
  "unterminated",
  "prose after valid insufficient plasm_context is legitimate clarification (unterminated)",
);
assert.equal(afterCalls, 2, "one valid discovery response then prose stop");
assert.ok(
  isForcedDiscoveryChoice(afterChoices[0]),
  "first step must still force plasm_context",
);
assert.ok(
  afterChoices[1] === undefined ||
    (typeof afterChoices[1] === "object" &&
      afterChoices[1] !== null &&
      (afterChoices[1] as { type?: string }).type !== "tool"),
  "after valid discovery response, force must clear (clarification may unterminate)",
);

console.log(
  "eval-terminal-discovery-gate: tool-error refusal, forced discovery before prose, " +
    "malformed plasm_context does not satisfy, clarification after valid response — sealed",
);
