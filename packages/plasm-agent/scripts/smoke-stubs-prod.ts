#!/usr/bin/env node
/**
 * Smoke: dynamic import of compiled `.mjs` stub (Vercel prod path).
 */
import path from "node:path";
import { pathToFileURL } from "node:url";

import { createFixtureMockTransport } from "../src/engine/fixture-mock-transport.js";

const projectRoot = process.argv[2] ?? process.cwd();
const entryId = process.argv[3] ?? "execute_tiny";

async function main(): Promise<void> {
  process.env.PLASM_STUB_USE_MOCK_TRANSPORT = "1";

  const stubPath = path.join(projectRoot, "agent/.plasm/stubs", `${entryId}.mjs`);
  const mod = (await import(pathToFileURL(stubPath).href)) as {
    product_list?: (opts?: { transport?: unknown }) => Promise<Array<{ id?: string }>>;
    item_search?: (input: {
      query: string;
      tags?: string;
      per_page?: number;
    }) => Promise<Array<{ id?: number | string; title?: string }>>;
  };

  const mock = createFixtureMockTransport();

  if (typeof mod.product_list === "function") {
    const rows = await mod.product_list({ transport: mock });
    if (!rows.length) throw new Error("product_list returned no rows");
    console.log(`OK: compiled stub product_list (${rows.length} rows)`);
    return;
  }

  if (typeof mod.item_search === "function") {
    const rows = await mod.item_search({
      query: "MCP OR Model Context Protocol",
      tags: "story",
      per_page: 3,
    });
    if (!rows.length) throw new Error("item_search returned no rows");
    console.log(`OK: compiled stub item_search (${rows.length} hits)`);
    return;
  }

  throw new Error(`stub ${entryId}.mjs missing product_list or item_search export`);
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
