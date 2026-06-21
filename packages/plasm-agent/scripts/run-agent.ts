#!/usr/bin/env node
/**
 * Local dev runner: one agent turn from argv or stdin.
 *
 *   npm run agent -- "list products from the catalog"
 */
import path from "node:path";
import { fileURLToPath } from "node:url";

import { createPlasmAgent } from "../agent/agent.js";

async function readPrompt(): Promise<string> {
  const arg = process.argv.slice(2).join(" ").trim();
  if (arg) return arg;
  return new Promise((resolve, reject) => {
    let buf = "";
    process.stdin.setEncoding("utf8");
    process.stdin.on("data", (chunk) => {
      buf += chunk;
    });
    process.stdin.on("end", () => resolve(buf.trim()));
    process.stdin.on("error", reject);
  });
}

async function main() {
  const prompt = await readPrompt();
  if (!prompt) {
    console.error("usage: run-agent.ts <prompt>");
    process.exit(1);
  }

  const agent = await createPlasmAgent();
  const result = await agent.generate(prompt);
  console.log(result.text);
  if (process.env.PLASM_AGENT_DEBUG_STEPS === "1") {
    console.error(JSON.stringify({ steps: result.steps.length, usage: result.usage }, null, 2));
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
