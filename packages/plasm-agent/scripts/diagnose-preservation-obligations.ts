/** Offline boundary probe, not evidence of model behavior or a semantic verifier. */
import assert from "node:assert/strict";
import { TaskLedgerStore, createTaskLedgerTool, TASK_LEDGER_TOOL_NAME, taskLedgerInputSchema } from "../src/tools/task-ledger.js";

const cases = [
  { outcome: "Keep the original routing membership", action: "Detach route", effect: "Removes the selected route from the active set", before: "Route A is active", intermediate: "Route A is absent" },
  { outcome: "Preserve the original access grant", action: "Suspend grant", effect: "Disables the selected grant", before: "Grant A is enabled", intermediate: "Grant A is disabled" },
  { outcome: "Retain the original reservation", action: "Release reservation", effect: "Removes the selected reservation", before: "Reservation A exists", intermediate: "Reservation A is absent" },
];
const observationRef = `pr${"a".repeat(64)}`;
const effectRef = `pr${"b".repeat(64)}`;
for (const scenario of cases) {
  const store = new TaskLedgerStore();
  const status = () => store.record.obligations[0]?.status;
  const ledger = createTaskLedgerTool(store)[TASK_LEDGER_TOOL_NAME]!;
  const write = async (record: unknown) => ledger.execute!(
    taskLedgerInputSchema.parse({ action: "write", record }),
    { toolCallId: "abstract-boundary", messages: [], context: {} },
  );
  await write({
    scope: scenario.outcome,
    observed_facts: [{ kind: "fact", text: scenario.before, evidence_ref: observationRef }],
    obligations: [
      { id: 1, task: scenario.outcome, status: "pending" },
      { id: 2, task: scenario.action, status: "pending" },
    ],
  });
  await write({
    completed_effects: [{ effect: scenario.effect, evidence_ref: effectRef, satisfaction: "interpretation" }],
    obligations: [{ id: 2, task: scenario.action, status: "resolved", evidence_ref: effectRef }],
  });
  assert.equal(status(), "pending", "action completion cannot implicitly discharge another obligation");
  assert.equal(store.record.observed_facts[0]?.text, scenario.before);
  await write({ obligations: [{ id: 1, task: scenario.outcome, status: "deferred", reason: "Temporary change remains; final state not verified" }] });
  await write({ obligations: [] });
  assert.equal(status(), "deferred");

  // A syntactically valid receipt cannot discharge unverified preservation.
  await write({
    observed_facts: [{ kind: "fact", text: scenario.intermediate, evidence_ref: effectRef }],
    obligations: [{ id: 1, task: scenario.outcome, status: "resolved", evidence_ref: effectRef }],
  });
  assert.equal(status(), "deferred");
  console.log(JSON.stringify({ case: scenario.action, separate_obligation_retained: true, explicit_false_resolution_admitted: false }));
}
console.log("PROVEN: intermediate effect receipts do not resolve unverified obligations.");
