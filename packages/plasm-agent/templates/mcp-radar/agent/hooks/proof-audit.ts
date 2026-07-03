import { defineHook } from "@plasm_lang/vercel-agent";

import { recordRunAudit } from "../../lib/run-audit.js";

export default defineHook({
  name: "proof-audit",
  on: ["run:complete", "plan:commit"],
  handler: (_ctx, detail) => {
    if (!detail || typeof detail !== "object") return;
    recordRunAudit(detail as Record<string, unknown>);
    const d = detail as Record<string, unknown>;
    console.log(
      "[mcp-radar:hook:proof-audit]",
      JSON.stringify({
        event: "audit",
        runId: d.runId,
        planCommitRef: d.planCommitRef ?? d.runRef,
        logicalSessionRef: d.logicalSessionRef,
        intent: d.intent,
        ok: d.ok,
      }),
    );
  },
});
