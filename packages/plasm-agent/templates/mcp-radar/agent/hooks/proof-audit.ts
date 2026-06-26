import { defineHook } from "@plasm_lang/vercel-agent";

export default defineHook({
  name: "proof-audit",
  on: ["run:complete", "plan:commit"],
  handler: (_ctx, detail) => {
    const keys = detail && typeof detail === "object" ? Object.keys(detail) : [];
    console.log(`[mcp-radar:hook:proof-audit] event detail keys=${keys.join(",")}`);
  },
});
