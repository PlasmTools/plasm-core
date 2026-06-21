#!/usr/bin/env node
/**
 * Deterministic tool-chain smoke (no LLM): context → dry-run → live run.
 */
import { createPlasmAgent } from "../agent/agent.js";

const INTENT = "list execute_tiny products";

function extractPlanCommitRef(markdown: string): string {
  const match = markdown.match(/\b(pc\d+)\b/);
  if (!match) {
    throw new Error(`plan_commit_ref not found in dry-run output:\n${markdown}`);
  }
  return match[1];
}

function extractLogicalSessionRef(markdown: string): string {
  const match = markdown.match(/^`([^`]+)`/m);
  if (!match) {
    throw new Error(`logical_session_ref not found in context output:\n${markdown}`);
  }
  return match[1];
}

async function main(): Promise<void> {
  const agent = await createPlasmAgent();
  await agent.bootstrap();

  const contextMd = await agent.runtime.plasmContext({
    intent: INTENT,
    seeds: [{ api: "execute_tiny", entity: "Product" }],
  });
  const logicalSessionRef = extractLogicalSessionRef(contextMd);
  console.log("--- plasm_context ---");
  console.log(contextMd);

  const dryMd = await agent.runtime.plasm({
    logicalSessionRef,
    program: "e1",
  });
  const planCommitRef = extractPlanCommitRef(dryMd);
  console.log("--- plasm (dry) ---");
  console.log(dryMd);

  const runMd = await agent.runtime.plasmRun({
    logicalSessionRef,
    planCommitRef,
  });
  console.log("--- plasm_run (live) ---");
  console.log(runMd);

  if (!/Widget|p1/i.test(runMd)) {
    throw new Error("live run output missing fixture product rows");
  }
  console.log("\nOK: execute_tiny tool chain completed.");
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
