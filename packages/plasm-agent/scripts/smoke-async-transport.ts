#!/usr/bin/env node
/**
 * Smoke: async host transport (Promise return) through NAPI live run.
 */
import { createPlasmAgent } from "../agent/agent.js";

function extractPlanCommitRef(markdown: string): string {
  const match = markdown.match(/\b(pc\d+)\b/);
  if (!match) throw new Error(`plan_commit_ref not found:\n${markdown}`);
  return match[1];
}

function extractLogicalSessionRef(markdown: string): string {
  const match = markdown.match(/^`([^`]+)`/m);
  if (!match) throw new Error(`logical_session_ref not found:\n${markdown}`);
  return match[1];
}

async function main(): Promise<void> {
  const agent = await createPlasmAgent();
  await agent.bootstrap();

  const contextMd = await agent.runtime.plasmContext({
    intent: "list execute_tiny products async transport",
    seeds: [{ api: "execute_tiny", entity: "Product" }],
  });
  const logicalSessionRef = extractLogicalSessionRef(contextMd);

  const dryMd = await agent.runtime.plasm({
    logicalSessionRef,
    program: "e1",
  });
  const planCommitRef = extractPlanCommitRef(dryMd);

  const runMd = await agent.runtime.plasmRun({
    logicalSessionRef,
    planCommitRef,
  });

  if (!/Widget|p1/i.test(runMd)) {
    throw new Error(`async transport live run missing fixture rows:\n${runMd}`);
  }
  console.log("OK: async Promise host transport completed.");
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
