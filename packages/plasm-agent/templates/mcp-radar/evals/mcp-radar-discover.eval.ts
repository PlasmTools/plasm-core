import { defineEval } from "@plasm_lang/vercel-agent";

export default defineEval({
  name: "mcp-radar-discover",
  goal: [
    "Search Hacker News for recent discussion about MCP (Model Context Protocol).",
    "Open a federated plasm session for hackernews and tavily if available.",
    "Summarize one interesting story and what MCP innovation it represents.",
  ].join(" "),
  assert: {
    toolsUsedAny: ["plasm_context", "plasm_run"],
    minSteps: 1,
  },
});
