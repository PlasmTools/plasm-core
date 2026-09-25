#!/usr/bin/env node
/**
 * Smoke: async host transport (Promise return) through NAPI live run.
 */
import { createPlasmAgent } from "../agent/agent.js";
import { runRefsInText } from "../src/telemetry/eve-tool-loop.js";

function extractPlanCommitRef(markdown: string): string {
  const ref = runRefsInText(markdown)[0];
  if (!ref) throw new Error(`run_ref not found:\n${markdown}`);
  return ref;
}

function extractLogicalSessionRef(markdown: string): string {
  const match = markdown.match(/\*\*logical_session_ref:\*\*\s*`([^`]+)`/i);
  if (!match?.[1]) throw new Error(`logical_session_ref not found:\n${markdown}`);
  return match[1];
}

async function main(): Promise<void> {
  const agent = await createPlasmAgent();
  await agent.bootstrap();

  const contextMd = await agent.runtime.plasmContext({
    intent: "list execute_tiny products async transport",
  });
  const logicalSessionRef = extractLogicalSessionRef(contextMd);

  const dryMd = await agent.runtime.plasm({
    logicalSessionRef,
    program: "class Read(Program):\n    def build(self):\n        return e1.query()",
  });
  const planCommitRef = extractPlanCommitRef(dryMd);

  const runMd = await agent.runtime.plasmRun({
    logicalSessionRef,
    runRef: planCommitRef,
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
