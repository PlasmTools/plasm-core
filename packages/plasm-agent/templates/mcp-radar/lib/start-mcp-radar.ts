import type { AuthoringContext } from "@plasm_lang/vercel-agent";

import type { RadarRunOptions } from "./run-radar.js";

/** Must match @workflow/nitro manifest for `workflows/mcp-radar-scan.ts`. */
const MCP_RADAR_SCAN_WORKFLOW_ID =
  "workflow//./workflows/mcp-radar-scan//mcpRadarScanWorkflow";

export async function startMcpRadarRun(
  ctx: AuthoringContext,
  options: RadarRunOptions = {},
): Promise<unknown> {
  const { start } = await import("workflow/api");
  return start({ workflowId: MCP_RADAR_SCAN_WORKFLOW_ID }, [
    ctx.agentRoot,
    options.force === true,
    options.reset === true,
  ]);
}
