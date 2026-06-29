import type { AuthoringContext } from "@plasm_lang/vercel-agent";

import type { RadarRunOptions } from "./run-radar.js";

export async function startMcpRadarRun(
  ctx: AuthoringContext,
  options: RadarRunOptions = {},
): Promise<unknown> {
  const { start } = await import("workflow/api");
  const { mcpRadarScanWorkflow } = await import("../workflows/mcp-radar-scan.js");
  return start(mcpRadarScanWorkflow, [ctx.agentRoot, options.force === true]);
}
