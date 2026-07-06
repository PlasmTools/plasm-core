import { getWorkflowMetadata } from "workflow";

/** Session + turn in one workflow — stream writes to session `getWritable()` (Eve parent pattern). */
export async function mcpRadarSessionWorkflow(
  agentRoot: string,
  force = false,
  reset = false,
  channelKind = "schedule",
) {
  "use workflow";
  const { workflowRunId } = getWorkflowMetadata();
  return executeMcpRadarTurnStep(workflowRunId, agentRoot, force, reset, channelKind);
}

async function executeMcpRadarTurnStep(
  sessionRunId: string,
  agentRoot: string,
  force: boolean,
  reset: boolean,
  channelKind: string,
) {
  "use step";
  const { getWritable } = await import("workflow");
  const writable = getWritable();
  const { runEveWorkflowTurn } = await import("@plasm_lang/vercel-agent/eve-turn");
  const { buildRadarGoal } = await import("../lib/run-radar.js");
  return runEveWorkflowTurn({
    agentRoot,
    sessionRunId,
    channelKind,
    userMessage: buildRadarGoal({ force, reset }),
    writable,
  });
}
