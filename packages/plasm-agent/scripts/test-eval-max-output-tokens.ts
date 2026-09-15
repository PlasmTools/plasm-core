#!/usr/bin/env node
/**
 * PLASM_EVAL_MAX_OUTPUT_TOKENS: default only when unset; reject explicit invalids.
 */
import assert from "node:assert/strict";
import {
  DEFAULT_EVAL_MAX_OUTPUT_TOKENS,
  resolveEvalMaxOutputTokens,
} from "../src/eval-max-output-tokens.js";

assert.equal(DEFAULT_EVAL_MAX_OUTPUT_TOKENS, 16_384);
assert.equal(resolveEvalMaxOutputTokens(undefined), 16_384, "unset → default");
assert.equal(resolveEvalMaxOutputTokens("8192"), 8192);
assert.equal(resolveEvalMaxOutputTokens("  4096  "), 4096, "trim whitespace");

for (const bad of ["0", "-1", "1.5", "abc", "", "  ", "NaN", "Infinity"]) {
  assert.throws(
    () => resolveEvalMaxOutputTokens(bad),
    /PLASM_EVAL_MAX_OUTPUT_TOKENS is set but invalid/,
    `must reject ${JSON.stringify(bad)}`,
  );
}

console.log("ok: eval-max-output-tokens");
