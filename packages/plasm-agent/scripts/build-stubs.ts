#!/usr/bin/env node
/**
 * Scan agent/catalogs/ and emit TypeScript stubs to agent/.plasm/stubs/<entry_id>.ts
 */
import path from "node:path";
import { fileURLToPath } from "node:url";

import { generateAllStubs } from "../src/stubs/generator.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const agentRoot = path.join(packageRoot, "agent");

async function main() {
  const results = await generateAllStubs(agentRoot);
  if (!results.length) {
    console.error("no catalogs found under agent/catalogs/");
    process.exit(1);
  }
  for (const result of results) {
    console.log(`${result.entryId}\t${result.catalogCgsHash}\t${result.outPath}`);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
