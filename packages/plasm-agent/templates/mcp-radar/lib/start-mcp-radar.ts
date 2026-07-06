import type { AuthoringContext } from "@plasm_lang/vercel-agent";
import {
  buildSessionAttributes,
  stringifyEveWorkflowAttributes,
} from "@plasm_lang/vercel-agent/eve-workflow";

import type { RadarRunOptions } from "./run-radar.js";

/** Must match @workflow/nitro manifest for `workflows/mcp-radar-session.ts`. */
const MCP_RADAR_SESSION_WORKFLOW_ID =
  "workflow//./workflows/mcp-radar-session//mcpRadarSessionWorkflow";

export async function startMcpRadarRun(
  ctx: AuthoringContext,
  options: RadarRunOptions = {},
): Promise<unknown> {
  const { start } = await import("workflow/api");
  const channelKind = options.channelKind ?? "schedule";
  return start(
    { workflowId: MCP_RADAR_SESSION_WORKFLOW_ID },
    [ctx.agentRoot, options.force === true, options.reset === true, channelKind],
    {
      attributes: stringifyEveWorkflowAttributes(
        buildSessionAttributes({
          channelKind,
          title: "MCP radar scan",
        }),
      ),
      allowReservedAttributes: true,
    },
  );
}
