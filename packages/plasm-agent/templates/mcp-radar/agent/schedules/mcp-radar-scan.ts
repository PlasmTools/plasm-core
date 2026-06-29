import { defineSchedule } from "@plasm_lang/vercel-agent";

import { startMcpRadarRun } from "../../lib/start-mcp-radar.js";

export default defineSchedule({
  name: "mcp-radar-scan",
  cron: "0 */6 * * *",
  handler: async (ctx) => {
    await startMcpRadarRun(ctx);
    console.log("[mcp-radar:schedule] workflow started");
  },
});
