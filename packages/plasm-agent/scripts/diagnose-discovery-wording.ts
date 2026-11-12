/** Model-free interface diagnostic. No runtime wording changes or provider calls. */
import assert from "node:assert/strict";
import { formatPlasmContextMarkdown } from "../src/tools/format.js";
import { routingExplanationLines } from "../src/engine/routing.js";

const states = [
  { id: "already_available", unexplored: 0, deferred: 0, repeated: false, changed: false },
  { id: "unexplored", unexplored: 4, deferred: 0, repeated: false, changed: false },
  { id: "byte_deferred", unexplored: 4, deferred: 4, repeated: false, changed: false },
  { id: "repeated_no_progress", unexplored: 4, deferred: 4, repeated: true, changed: false },
  { id: "changed_intent", unexplored: 4, deferred: 4, repeated: false, changed: true },
];
// These are diagnostic states, not evidence that the caller's goal is satisfied.
// The production formatter receives no search receipt or progress history.
const rows = states.map(state => ({
  ...state,
  text: formatPlasmContextMarkdown("l_fixture", "", "unchanged"),
}));
const distinctResponses = new Set(rows.map(row => row.text)).size;
assert.equal(distinctResponses, 1, "Update the diagnostic when the response contract changes");
const noSelection = routingExplanationLines({ matches: [] });
assert.ok(noSelection.join(" ").includes("current bounded packet"));
assert.ok(rows.every(row => row.text.includes("Previously exposed declarations")));
console.log(JSON.stringify({
  diagnosticOnly: true, providerCalls: 0, stateCount: rows.length, distinctResponses,
  noSelection, rows,
  finding: "The unchanged-declaration formatter cannot distinguish availability, unexplored work, byte deferral, repeated no-progress, or changed intent. This is information loss, not a measured caller-behaviour effect.",
  limits: "The full runtime may additionally emit native recovery guidance. This diagnostic covers the normal unchanged-declaration branch; it does not claim every response is identical.",
}, null, 2));
