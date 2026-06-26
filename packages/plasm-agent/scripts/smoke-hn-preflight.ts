#!/usr/bin/env node
/**
 * Smoke: live hackernews item_search via generated stub (skips when offline).
 */
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const projectRoot = process.argv[2] ?? process.cwd();
const stubPath = path.join(projectRoot, "agent/.plasm/stubs/hackernews.ts");

async function main(): Promise<void> {
  const stubUrl = pathToFileURL(stubPath).href;
  const mod = (await import(stubUrl)) as {
    item_search: (input: {
      query: string;
      tags?: string;
      per_page?: number;
    }) => Promise<Array<{ id?: number; title?: string }>>;
  };

  try {
    const rows = await mod.item_search({
      query: "MCP OR Model Context Protocol",
      tags: "story",
      per_page: 3,
    });
    if (!rows.length) {
      throw new Error("item_search returned zero rows");
    }
    console.log(
      `OK: HN preflight (${rows.length} hits, first: ${rows[0]?.title?.slice(0, 60) ?? rows[0]?.id})`,
    );
  } catch (err) {
    const message = String(err);
    if (
      message.includes("fetch failed") ||
      message.includes("ENOTFOUND") ||
      message.includes("ETIMEDOUT")
    ) {
      console.log("SKIP: HN preflight (offline or network blocked)");
      return;
    }
    throw err;
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
