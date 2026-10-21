import assert from "node:assert/strict";
import { formatPlasmContextMarkdown } from "../src/tools/format.js";

const declaration = "class Item: ...";
const guidance = "Bind the credential from the declared source.";
for (const [exposure, prompt, expected] of [
  ["initial", declaration, "Initial declarations"],
  ["extended", declaration, "Additional declarations"],
  ["unchanged", guidance, "No new declarations"],
  ["unchanged", "", "No new declarations"],
] as const) {
  const result = formatPlasmContextMarkdown("l_abstract", prompt, exposure);
  assert.ok(result.includes(expected));
  assert.ok(result.includes("does not read or refresh account data"));
  if (prompt) assert.ok(result.includes(prompt));
  assert.equal(result.includes("Additional declarations"), exposure === "extended");
  assert.ok(result.includes("l_abstract"));
}
console.log("context exposure matrix passed");
