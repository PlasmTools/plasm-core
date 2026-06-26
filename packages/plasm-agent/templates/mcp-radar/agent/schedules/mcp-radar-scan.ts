import { defineSchedule } from "@plasm_lang/vercel-agent";

import { runRadar } from "../../lib/run-radar.js";

export default defineSchedule({
  name: "mcp-radar-scan",
  cron: "0 */6 * * *",
  handler: async (ctx) => {
    const result = await runRadar(ctx);
    console.log(
      `[mcp-radar:schedule] ok=${result.ok} skipped=${result.skipped} new=${result.newCandidates.length} reason=${result.reason ?? ""}`,
    );
  },
});
