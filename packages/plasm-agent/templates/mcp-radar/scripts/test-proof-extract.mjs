#!/usr/bin/env node
import assert from "node:assert/strict";
import { extractProofMarkdown, isMcpRelevant } from "../lib/proof-extract.js";

const raw = `
The session expired after plasm_context. Tavily is unavailable.

### [MCP Server Guide](https://news.ycombinator.com/item?id=123)
- **HN**: id 123, score 42
- **Synthesis**: Example MCP tooling.
- **Confidence**: low

Some trailing narration about errors.
`;

const extracted = extractProofMarkdown(raw);
assert.match(extracted, /^### \[MCP Server Guide\]/);
assert.doesNotMatch(extracted, /session expired/i);
assert.doesNotMatch(extracted, /trailing narration/i);

assert.equal(isMcpRelevant("New MCP server for agents", undefined), true);
assert.equal(isMcpRelevant("Stephen Hawking has died", undefined), false);

console.log("proof-extract: ok");
