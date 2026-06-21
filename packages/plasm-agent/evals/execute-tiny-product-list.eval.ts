import { defineEval } from "../src/evals/define-eval.js";

export default defineEval({
  name: "execute-tiny-product-list",
  goal: "Use the execute_tiny catalog to list products and summarize what you found.",
  assert: {
    toolsUsedAny: ["plasm_context", "plasm_run"],
    minSteps: 1,
  },
});
