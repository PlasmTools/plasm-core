import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import { buildDefaultSystemLiturgy } from "../src/prompts/index.js";

const old = "An alternative action is authorized only when the user’s stated condition is met. A failed preferred action is not that condition.";
const revised = "Different means may achieve the same authorized outcome if they respect the user's constraints and preserve unrelated state. If the user makes an alternative conditional, establish that condition before taking it; do not substitute failure of one attempt for a different stated condition. Uncertain authorization remains unresolved; do not expand scope.";
const current = buildDefaultSystemLiturgy({ includeTaskLedger: true });
assert.equal(current.split(revised).length - 1, 1);
assert.ok(!current.includes(old));
assert.ok(!buildDefaultSystemLiturgy({ includeTaskLedger: false }).includes(revised));
assert.ok(current.includes("unfinished restoration or verification"));
assert.ok(current.includes("Write `interpretations` separately"));
if (process.argv[2]) {
  writeFileSync(process.argv[2], JSON.stringify({
    before: current.replace(revised, old), after: current,
  }, null, 2));
}
console.log("authorization distinction: exact delivery, gate and preservation checks passed");
