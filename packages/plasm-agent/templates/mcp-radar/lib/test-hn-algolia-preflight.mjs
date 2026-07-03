#!/usr/bin/env node
import assert from "node:assert/strict";

import { fetchHnMcpStoriesByDate } from "../lib/hn-algolia-preflight.js";
import { isMcpRelevant } from "../lib/proof-extract.js";
import { MCP_SEARCH_QUERY, preflightHnMcpStories } from "../lib/run-radar.js";

const rows = await fetchHnMcpStoriesByDate({
  query: MCP_SEARCH_QUERY,
  tags: "story",
  perPage: 10,
});
assert.ok(rows.length > 0, "Algolia should return hits for MCP query");

const mcpRows = rows.filter((r) => isMcpRelevant(r.title, r.url));
assert.ok(mcpRows.length > 0, "At least one hit should be MCP-relevant");
assert.equal(
  mcpRows.some((r) => /hawking|altman|crowdstrike/i.test(r.title ?? "")),
  false,
  "Preflight must not return generic top HN stories",
);

const candidates = await preflightHnMcpStories();
assert.ok(candidates.length > 0, "preflightHnMcpStories should yield MCP candidates");
console.log(
  `hn-algolia-preflight: ok (${candidates.length} MCP candidates, first: ${candidates[0]?.title?.slice(0, 60)})`,
);
