#!/usr/bin/env node
/**
 * T214120: when eval terminals are registered, liturgy must not teach
 * “Stop with no tool call.” T144504 those tasks used complete_task.
 * T214120 31dc/325 followed Voice A prose. Grade path unchanged: prose-stop
 * stays unterminated (see test-agent-conversation / test-graded-scalar-after-writes).
 *
 * Overlay is a named `WORKFLOW_COMPLETION_SLOT` replacement, not first-paragraph
 * index surgery. A decoy leading paragraph must survive; a missing slot must throw.
 */
import assert from "node:assert/strict";
import {
  EVAL_TERMINAL_COMPLETION,
  WORKFLOW_COMPLETION_SLOT,
  buildDefaultSystemLiturgy,
  overlayWorkflowCompletion,
} from "../src/prompts/index.js";
import { createHarnessTools } from "../src/tools/harness-tools.js";

const NO_TOOL_CALL = /Stop with no tool call/;

const product = buildDefaultSystemLiturgy();
assert.equal(
  product.includes(WORKFLOW_COMPLETION_SLOT),
  true,
  "product liturgy must keep the named completion-contract slot",
);
assert.match(product, NO_TOOL_CALL);
assert.equal(product.includes("complete_task"), false, "non-eval liturgy must not invent complete_task");
assert.equal(product.includes("submit_answer"), false, "non-eval liturgy must not invent submit_answer");
assert.match(product, /No prose/, "plasm_tool.txt program-text law remains");

const evalLiturgy = buildDefaultSystemLiturgy({ includeEvalTerminals: true });
assert.equal(
  evalLiturgy.includes(EVAL_TERMINAL_COMPLETION),
  true,
  "eval liturgy must carry the named eval completion contract",
);
assert.equal(
  evalLiturgy.includes(WORKFLOW_COMPLETION_SLOT),
  false,
  "eval liturgy must overlay the named slot, not keep the product contract",
);
assert.equal(
  NO_TOOL_CALL.test(evalLiturgy),
  false,
  "eval-terminal liturgy must not teach no-tool-call stop",
);
assert.match(evalLiturgy, /complete_task/);
assert.match(evalLiturgy, /submit_answer/);
assert.match(evalLiturgy, /no reportable value/);
assert.match(evalLiturgy, /Call `plasm_context`/, "workflow steps after the slot must remain");
assert.match(evalLiturgy, /No prose/, "plasm_tool.txt program-text law remains");
assert.equal(evalLiturgy.includes("halt-after"), false);

const decoyWorkflow =
  "# Plasm workflow\n\n" +
  "Decoy first paragraph that positional surgery would replace.\n\n" +
  WORKFLOW_COMPLETION_SLOT +
  "\n\n1. Call `plasm_context` leftover\n";
const decoyOverlaid = overlayWorkflowCompletion(decoyWorkflow, EVAL_TERMINAL_COMPLETION);
assert.match(
  decoyOverlaid,
  /Decoy first paragraph that positional surgery would replace/,
  "named-slot overlay must not replace a leading decoy paragraph",
);
assert.equal(decoyOverlaid.includes(EVAL_TERMINAL_COMPLETION), true);
assert.equal(decoyOverlaid.includes("Stop with no tool call"), false);
assert.match(decoyOverlaid, /Call `plasm_context` leftover/);

assert.throws(
  () => overlayWorkflowCompletion("# Title\n\nNo named slot here.\n", EVAL_TERMINAL_COMPLETION),
  /exactly one WORKFLOW_COMPLETION_SLOT/,
);
assert.throws(
  () =>
    overlayWorkflowCompletion(
      `${WORKFLOW_COMPLETION_SLOT}\n\n${WORKFLOW_COMPLETION_SLOT}`,
      EVAL_TERMINAL_COMPLETION,
    ),
  /exactly one WORKFLOW_COMPLETION_SLOT/,
);

const productTools = createHarnessTools({});
assert.equal("complete_task" in productTools, false);
assert.equal("submit_answer" in productTools, false);

const evalTools = createHarnessTools({ includeEvalTerminals: true });
assert.equal("complete_task" in evalTools, true);
assert.equal("submit_answer" in evalTools, true);

console.log("PASS: eval-terminal liturgy is Voice B; product liturgy does not invent terminals");
