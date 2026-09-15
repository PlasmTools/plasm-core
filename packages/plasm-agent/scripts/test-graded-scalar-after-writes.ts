#!/usr/bin/env node
import assert from "node:assert/strict";
import {
  COMPLETE_TASK_TOOL_DESCRIPTION,
  SUBMIT_ANSWER_TOOL_DESCRIPTION,
} from "../src/tools/harness-tools.js";
import {
  evalTerminalGrade,
  extractGradedScalarFromObservation,
  gradedScalarAfterLiveWrites,
  hostMayInjectAppWorldComplete,
  lastSuccessfulEvalTerminal,
  submittedAnswerFromSubmitAnswer,
  successfulEvalTerminalInStep,
  taskPrefersLabelAnswer,
  validSubmitAnswerPayload,
  writeCountFromSummary,
} from "../src/tools/format.js";

assert.notEqual(COMPLETE_TASK_TOOL_DESCRIPTION, "Mark the task complete.");
assert.notEqual(SUBMIT_ANSWER_TOOL_DESCRIPTION, "Submit the reportable value.");
assert.match(COMPLETE_TASK_TOOL_DESCRIPTION, /asked for no reportable value/);
assert.match(COMPLETE_TASK_TOOL_DESCRIPTION, /explicit null/);
assert.match(COMPLETE_TASK_TOOL_DESCRIPTION, /Do not call this when the instruction asked you to report a value/);
assert.match(SUBMIT_ANSWER_TOOL_DESCRIPTION, /asked for a reportable value/);
assert.match(SUBMIT_ANSWER_TOOL_DESCRIPTION, /verbatim/);
assert.match(SUBMIT_ANSWER_TOOL_DESCRIPTION, /monetary value/);
assert.match(SUBMIT_ANSWER_TOOL_DESCRIPTION, /only the numeric part/);
assert.match(SUBMIT_ANSWER_TOOL_DESCRIPTION, /no currency symbols or comma groupings/);
assert.match(SUBMIT_ANSWER_TOOL_DESCRIPTION, /Not a done-summary, not a table, not surrounding prose/);
assert.match(SUBMIT_ANSWER_TOOL_DESCRIPTION, /use complete_task instead/);
assert.match(SUBMIT_ANSWER_TOOL_DESCRIPTION, /empty or missing answer is invalid/i);
assert.ok(
  !/currency amount/i.test(SUBMIT_ANSWER_TOOL_DESCRIPTION),
  "do not invite a currency-formatted token",
);
assert.ok(
  !/\$/.test(SUBMIT_ANSWER_TOOL_DESCRIPTION),
  "do not invite $ as the submitted token",
);
assert.ok(
  !/Venmo|Stacy|024|genre|166/i.test(
    COMPLETE_TASK_TOOL_DESCRIPTION + SUBMIT_ANSWER_TOOL_DESCRIPTION,
  ),
  "terminal descriptions stay domain-general",
);

assert.equal(writeCountFromSummary("plan review · 5n 3r 2w"), 2);
assert.equal(writeCountFromSummary("plan ok · 2n 0r 0w"), 0);
assert.equal(writeCountFromSummary("no schedule"), 0);

assert.equal(taskPrefersLabelAnswer("which color is most common?"), true);
assert.equal(taskPrefersLabelAnswer("how many rows?"), false);

const ranking = "```tsv\ncolor\tn\nred\t12\nblue\t8\n```";
assert.equal(
  extractGradedScalarFromObservation(ranking, "which color is most common?"),
  "red",
);
assert.equal(
  extractGradedScalarFromObservation(ranking, "how many in the top bucket?"),
  "12",
);

const nBeforeTotal = "```tsv\nn\ttotal\n10\t471\n```";
assert.equal(
  extractGradedScalarFromObservation(nBeforeTotal, "how much was spent?"),
  "10",
  "extract still prefers n — that is why it must not replace the agent answer",
);
assert.equal(
  gradedScalarAfterLiveWrites({ text: "$471" }),
  "$471",
  "agent answer survives a multi-column extract trap",
);
assert.equal(
  gradedScalarAfterLiveWrites({ text: "Alice" }),
  "Alice",
  "Who/What answers survive",
);
assert.equal(gradedScalarAfterLiveWrites({ text: "  42  " }), "42");
assert.equal(gradedScalarAfterLiveWrites({ text: "   " }), null);
assert.equal(gradedScalarAfterLiveWrites({ text: "" }), null);
assert.equal(gradedScalarAfterLiveWrites({ text: null }), null);
assert.equal(gradedScalarAfterLiveWrites({}), null);

const doneNarration =
  "Done. I sent a public Venmo payment request for **$13** to **Stacy Maldonado**.";
const tableProse =
  "You've liked the most **classical** songs in your Spotify library — **12** of them.\n\n| Genre | Liked songs |\n| **classical** | **12** |";

assert.equal(
  submittedAnswerFromSubmitAnswer([
    { role: "assistant", content: [{ type: "text", text: doneNarration }] },
  ]),
  null,
  "chat prose without submit_answer is not an answer",
);
assert.equal(
  gradedScalarAfterLiveWrites({
    text: submittedAnswerFromSubmitAnswer([
      { role: "assistant", content: [{ type: "text", text: doneNarration }] },
    ]),
  }),
  null,
  "after live writes, omitted submit_answer grades as null",
);

assert.equal(
  submittedAnswerFromSubmitAnswer([
    {
      role: "assistant",
      content: [
        { type: "text", text: doneNarration },
        { type: "tool-call", toolCallId: "c0", toolName: "complete_task", input: {} },
      ],
    },
  ]),
  null,
  "complete_task alone grades as null",
);
assert.equal(
  submittedAnswerFromSubmitAnswer([
    {
      role: "assistant",
      content: [
        {
          type: "tool-call",
          toolCallId: "c1",
          toolName: "complete_task",
          input: { answer: doneNarration },
        },
      ],
    },
  ]),
  null,
  "leftover complete_task answer field is not graded",
);
assert.equal(
  gradedScalarAfterLiveWrites({ text: "" }),
  null,
  "empty omit grades as null",
);

assert.equal(
  submittedAnswerFromSubmitAnswer([
    {
      role: "assistant",
      content: [
        { type: "text", text: tableProse },
        {
          type: "tool-call",
          toolCallId: "c2",
          toolName: "submit_answer",
          input: { answer: "classical" },
        },
      ],
    },
  ]),
  "classical",
  "submit_answer(classical) is preserved; table leftover is not the answer",
);
assert.equal(
  gradedScalarAfterLiveWrites({ text: "classical" }),
  "classical",
);

assert.equal(
  submittedAnswerFromSubmitAnswer([
    {
      role: "assistant",
      content: [
        {
          type: "tool-call",
          toolCallId: "c-json",
          toolName: "submit_answer",
          input: '{"answer":"classical"}',
        },
      ],
    },
  ]),
  "classical",
);

assert.equal(validSubmitAnswerPayload({}), null);
assert.equal(validSubmitAnswerPayload({ answer: "" }), null);
assert.equal(validSubmitAnswerPayload({ answer: "   " }), null);
assert.equal(validSubmitAnswerPayload({ answer: "$471" }), "$471");
assert.equal(
  submittedAnswerFromSubmitAnswer([
    {
      role: "assistant",
      content: [{ type: "tool-call", toolCallId: "empty", toolName: "submit_answer", input: {} }],
    },
  ]),
  null,
  "invalid submit_answer({}) is not a submitted answer",
);
assert.equal(
  submittedAnswerFromSubmitAnswer([
    {
      role: "assistant",
      content: [
        { type: "tool-call", toolCallId: "ok", toolName: "submit_answer", input: { answer: "471" } },
        { type: "tool-call", toolCallId: "bad", toolName: "submit_answer", input: {} },
      ],
    },
  ]),
  "471",
  "invalid later submit must not overwrite a valid answer with null",
);
assert.equal(
  lastSuccessfulEvalTerminal([
    {
      role: "assistant",
      content: [{ type: "tool-call", toolCallId: "c-empty", toolName: "submit_answer", input: {} }],
    },
  ]),
  null,
  "call-only invalid submit is not successful termination",
);
assert.deepEqual(
  lastSuccessfulEvalTerminal([
    {
      role: "assistant",
      content: [{ type: "tool-call", toolCallId: "ct", toolName: "complete_task", input: {} }],
    },
    {
      role: "tool",
      content: [
        {
          type: "tool-result",
          toolCallId: "ct",
          toolName: "complete_task",
          output: { type: "text", value: "Task marked complete." },
        },
      ],
    },
  ]),
  { tool: "complete_task", answer: null },
);
assert.deepEqual(
  lastSuccessfulEvalTerminal([
    {
      role: "assistant",
      content: [
        {
          type: "tool-call",
          toolCallId: "sa",
          toolName: "submit_answer",
          input: { answer: "classical" },
        },
      ],
    },
    {
      role: "tool",
      content: [
        {
          type: "tool-result",
          toolCallId: "sa",
          toolName: "submit_answer",
          output: { type: "text", value: "Answer submitted." },
        },
      ],
    },
  ]),
  { tool: "submit_answer", answer: "classical" },
);
assert.equal(
  lastSuccessfulEvalTerminal([
    {
      role: "assistant",
      content: [
        { type: "tool-call", toolCallId: "bad", toolName: "submit_answer", input: {} },
      ],
    },
    {
      role: "tool",
      content: [
        {
          type: "tool-error",
          toolCallId: "bad",
          toolName: "submit_answer",
          isError: true,
          output: { type: "error-text", value: "invalid" },
        },
      ],
    },
  ]),
  null,
  "failed submit_answer execution is not successful termination",
);
assert.equal(
  successfulEvalTerminalInStep({
    toolCalls: [{ toolName: "submit_answer", toolCallId: "x", input: {} }],
    toolResults: [],
  }),
  null,
  "schema-miss with no result does not terminate",
);

assert.equal(
  submittedAnswerFromSubmitAnswer([
    {
      role: "assistant",
      content: [
        { type: "tool-call", toolCallId: "c-done", toolName: "complete_task", input: {} },
        { type: "text", text: tableProse },
      ],
    },
    {
      role: "assistant",
      content: [{ type: "text", text: doneNarration }],
    },
  ]),
  null,
  "leftover assistant text after complete_task must not override",
);
assert.equal(
  submittedAnswerFromSubmitAnswer([
    {
      role: "assistant",
      content: [
        {
          type: "tool-call",
          toolCallId: "c-sub",
          toolName: "submit_answer",
          input: { answer: "classical" },
        },
      ],
    },
    {
      role: "assistant",
      content: [{ type: "text", text: doneNarration }],
    },
  ]),
  "classical",
  "leftover streamText after submit_answer must not replace the submitted value",
);

const rankingTable = "```tsv\ngenre\tn\nclassical\t12\nindie\t9\n```";
assert.equal(
  extractGradedScalarFromObservation(rankingTable, "which genre have I liked the most?"),
  "classical",
  "extract still pulls the first label — that override stays forbidden on the live path",
);
assert.equal(
  gradedScalarAfterLiveWrites({
    text: submittedAnswerFromSubmitAnswer([
      {
        role: "assistant",
        content: [
          { type: "text", text: tableProse },
          {
            type: "tool-call",
            toolCallId: "c3",
            toolName: "submit_answer",
            input: { answer: "$471" },
          },
        ],
      },
    ]),
  }),
  "$471",
  "explicit agent answer is never replaced by a table extract",
);

const chatOnly = [
  { role: "assistant" as const, content: [{ type: "text", text: doneNarration }] },
];
const unterminatedChat = evalTerminalGrade({ messages: chatOnly, stopReason: "completed" });
assert.deepEqual(unterminatedChat, { kind: "unterminated" });
assert.equal(
  hostMayInjectAppWorldComplete(unterminatedChat),
  false,
  "T183304: leftover chat must not invent complete_task(status='success', answer='')",
);

const budgetFalloff = evalTerminalGrade({
  messages: chatOnly,
  stopReason: "budget_exhausted",
});
assert.deepEqual(budgetFalloff, { kind: "unterminated" });
assert.equal(
  hostMayInjectAppWorldComplete(budgetFalloff),
  false,
  "budget exhaustion is not a validated terminal",
);

const midBudgetProse = evalTerminalGrade({
  messages: chatOnly,
  stopReason: "unterminated",
});
assert.deepEqual(midBudgetProse, { kind: "unterminated" });
assert.equal(
  hostMayInjectAppWorldComplete(midBudgetProse),
  false,
  "mid-budget model stop is not a validated terminal",
);

const successfulComplete = [
  {
    role: "assistant" as const,
    content: [{ type: "tool-call", toolCallId: "ct", toolName: "complete_task", input: {} }],
  },
  {
    role: "tool" as const,
    content: [
      {
        type: "tool-result",
        toolCallId: "ct",
        toolName: "complete_task",
        output: { type: "text", value: "Task marked complete." },
      },
    ],
  },
];
const explicitNull = evalTerminalGrade({
  messages: successfulComplete,
  stopReason: "completed",
});
assert.deepEqual(explicitNull, { kind: "null", tool: "complete_task" });
assert.equal(hostMayInjectAppWorldComplete(explicitNull), true);

const successfulSubmit = [
  {
    role: "assistant" as const,
    content: [
      {
        type: "tool-call",
        toolCallId: "sa",
        toolName: "submit_answer",
        input: { answer: "$471" },
      },
    ],
  },
  {
    role: "tool" as const,
    content: [
      {
        type: "tool-result",
        toolCallId: "sa",
        toolName: "submit_answer",
        output: { type: "text", value: "Answer submitted." },
      },
    ],
  },
];
const scalar = evalTerminalGrade({
  messages: successfulSubmit,
  stopReason: "completed",
});
assert.deepEqual(scalar, { kind: "scalar", tool: "submit_answer", answer: "$471" });
assert.equal(hostMayInjectAppWorldComplete(scalar), true);
assert.equal(
  gradedScalarAfterLiveWrites({ text: scalar.kind === "scalar" ? scalar.answer : null }),
  "$471",
  "submit_answer($471) is preserved without extract/rewrite",
);

console.log("test-graded-scalar-after-writes: ok");
