#!/usr/bin/env node
/**
 * T214120: when eval terminals are registered, liturgy must not teach
 * “Stop with no tool call.” T144504 those tasks used complete_task.
 * T214120 31dc/325 followed Voice A prose. Grade path unchanged: prose-stop
 * stays unterminated (see test-agent-conversation / test-graded-scalar-after-writes).
 */
import assert from "node:assert/strict";
import { buildDefaultSystemLiturgy } from "../src/prompts/index.js";
import { createHarnessTools } from "../src/tools/harness-tools.js";

const NO_TOOL_CALL = /Stop with no tool call/;

const product = buildDefaultSystemLiturgy();
assert.match(product, NO_TOOL_CALL);
assert.equal(product.includes("complete_task"), false, "non-eval liturgy must not invent complete_task");
assert.equal(product.includes("submit_answer"), false, "non-eval liturgy must not invent submit_answer");
assert.match(product, /No prose/, "plasm_tool.txt program-text law remains");

const evalLiturgy = buildDefaultSystemLiturgy({ includeEvalTerminals: true });
assert.equal(
  NO_TOOL_CALL.test(evalLiturgy),
  false,
  "eval-terminal liturgy must not teach no-tool-call stop",
);
assert.match(evalLiturgy, /complete_task/);
assert.match(evalLiturgy, /submit_answer/);
assert.match(evalLiturgy, /no reportable value/);
assert.match(evalLiturgy, /No prose/, "plasm_tool.txt program-text law remains");
assert.equal(evalLiturgy.includes("halt-after"), false);

const productTools = createHarnessTools({});
assert.equal("complete_task" in productTools, false);
assert.equal("submit_answer" in productTools, false);

const evalTools = createHarnessTools({ includeEvalTerminals: true });
assert.equal("complete_task" in evalTools, true);
assert.equal("submit_answer" in evalTools, true);

console.log("PASS: eval-terminal liturgy is Voice B; product liturgy does not invent terminals");
