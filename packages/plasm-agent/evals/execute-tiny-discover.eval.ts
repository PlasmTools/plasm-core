import { defineEval } from "../src/evals/define-eval.js";

export default defineEval({
  name: "execute-tiny-discover",
  goal: "Find which execute_tiny entities can list or search products, then fetch the product list.",
  assert: {
    toolsUsedAny: ["discover_capabilities", "plasm_context"],
    minSteps: 1,
  },
});
