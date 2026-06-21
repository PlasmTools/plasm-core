#!/usr/bin/env node
/**
 * Smoke: typed stub dry-run + live execute against execute_tiny fixture.
 */
import path from "node:path";
import { fileURLToPath } from "node:url";

import { createFixtureMockTransport } from "../src/engine/fixture-mock-transport.js";
import { dryRunProgram } from "../src/stubs/catalog-client.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

async function main(): Promise<void> {
  process.env.PLASM_STUB_USE_MOCK_TRANSPORT = "1";

  const stubPath = path.join(packageRoot, "agent/.plasm/stubs/execute_tiny.ts");
  const mod = await import(stubPath);

  if (typeof mod.product_list !== "function") {
    throw new Error("execute_tiny stub missing product_list export");
  }
  if (typeof mod.product_get !== "function") {
    throw new Error("execute_tiny stub missing product_get export");
  }

  const dry = await dryRunProgram(mod.execute_tiny.builder, "e2");
  if (!/^pc\d+$/i.test(dry.planCommitRef)) {
    throw new Error(`unexpected plan_commit_ref: ${dry.planCommitRef}`);
  }
  console.log("dry-run product_list:", dry.planCommitRef);

  const mock = createFixtureMockTransport();
  const rows = await mod.product_list({ transport: mock });
  if (!Array.isArray(rows) || rows.length === 0) {
    throw new Error("product_list returned no rows");
  }
  console.log("live product_list rows:", rows.length, rows[0]);

  const one = await mod.product_get(
    { id: String(rows[0]?.id ?? "p1") },
    { transport: mock },
  );
  if (!one?.id) {
    throw new Error("product_get returned empty row");
  }
  console.log("live product_get:", one);

  console.log("\nOK: typed stubs execute");
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
