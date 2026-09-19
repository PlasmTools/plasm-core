#!/usr/bin/env node
/**
 * Deterministic tool-chain smoke (no LLM): context → dry-run → live run.
 */
import { createPlasmAgent } from "../agent/agent.js";
import { runRefsInText } from "../src/telemetry/eve-tool-loop.js";

const INTENT = "list execute_tiny products";

function extractRunRef(markdown: string): string {
  const ref = runRefsInText(markdown)[0];
  if (!ref) throw new Error(`run_ref not found:\n${markdown}`);
  return ref;
}

function extractLogicalSessionRef(markdown: string): string {
  const match = markdown.match(/\*\*logical_session_ref:\*\*\s*`([^`]+)`/i);
  if (!match?.[1]) {
    throw new Error(`logical_session_ref not found in context output:\n${markdown}`);
  }
  return match[1];
}

async function main(): Promise<void> {
  const agent = await createPlasmAgent();
  await agent.bootstrap();

  const contextMd = await agent.runtime.plasmContext({
    intent: INTENT,
    effectSlots: [INTENT],
  });
  const logicalSessionRef = extractLogicalSessionRef(contextMd);
  console.log("--- plasm_context ---");
  console.log(contextMd);

  const dryMd = await agent.runtime.plasm({
    logicalSessionRef,
    program: "e1",
  });
  const runRef = extractRunRef(dryMd);
  console.log("--- plasm (dry) ---");
  console.log(dryMd);

  const runMd = await agent.runtime.plasmRun({
    logicalSessionRef,
    runRef,
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
