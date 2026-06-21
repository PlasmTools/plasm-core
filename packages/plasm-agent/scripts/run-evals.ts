#!/usr/bin/env node
/**
 * Live LLM eval harness — requires Vercel AI Gateway (no fixture-only chains).
 */
import path from "node:path";
import { fileURLToPath } from "node:url";

import { createPlasmAgent } from "../agent/agent.js";
import {
  requireLiveEvalGateway,
  runAllEvals,
} from "../src/evals/run-eval.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const evalsDir = path.join(packageRoot, "evals");

async function main(): Promise<void> {
  requireLiveEvalGateway();

  const agent = await createPlasmAgent();
  await agent.bootstrap();

  const results = await runAllEvals(agent, evalsDir);
  if (!results.length) {
    console.warn("No evals found in", evalsDir);
    process.exit(0);
  }

  let failed = 0;
  for (const result of results) {
    if (result.ok) {
      console.log(
        `PASS ${result.name} steps=${result.stepCount} tools=[${result.recordedTools.join(", ")}]`,
      );
    } else {
      failed += 1;
      console.error(`FAIL ${result.name}: ${result.error ?? "unknown error"}`);
      console.error(`  tools=[${result.recordedTools.join(", ")}] steps=${result.stepCount}`);
    }
  }

  if (failed > 0) {
    process.exit(1);
  }
  console.log(`\nOK: ${results.length} live eval(s) passed`);
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
