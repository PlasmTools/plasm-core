import { type AuthoringContext } from "@plasm_lang/vercel-agent";

import { radarArchiveSummary } from "./radar-archive-status.js";
import { drainRunAudit, resetRunAudit } from "./run-audit.js";
import { gatewayConfigured, tavilyConfigured } from "./radar-state.js";

/** Stable intent — same string on every radar turn (`plasm_context` session reuse). */
export const MCP_RADAR_INTENT =
  "track MCP innovations from Hacker News and corroborate with Tavily web search";

export interface RadarRunOptions {
  force?: boolean;
  reset?: boolean;
  /** Eve Agent Runs `eve.channel.kind` (default `schedule`). */
  channelKind?: string;
}

export interface RadarRunResult {
  ok: boolean;
  skipped: boolean;
  reason?: string;
  agentText?: string;
  error?: string;
}

export function buildRadarGoal(options: RadarRunOptions): string {
  const lines = [
    "Run one MCP radar scan cycle.",
    "",
    "Use the Plasm tool loop only: `plasm_context` → `plasm` → `plasm_run`.",
    "Follow `agent/instructions.md` and skills `mcp-proof-format`, `proof-publish`.",
    "HN search, Tavily corroboration, and Proof document read/write are Plasm programs in your session — not TypeScript, not stubs, not chat-only markdown.",
    "",
    `Reuse this intent on every plasm_context call: ${JSON.stringify(MCP_RADAR_INTENT)}`,
  ];
  if (options.force === true) {
    lines.push("", "Force: include MCP HN stories even if the Proof doc already mentions them.");
  }
  if (options.reset === true) {
    lines.push("", "Reset: clear the Proof document to `# MCP Innovations Proof Log` before new entries.");
  }
  return lines.join("\n");
}

/** Start the agent loop; all catalog work happens inside Plasm session tools. */
export async function runRadar(
  ctx: AuthoringContext,
  options: RadarRunOptions = {},
): Promise<RadarRunResult> {
  if (!gatewayConfigured()) {
    const error =
      "AI Gateway is not configured (link project on Vercel or set AI_GATEWAY_API_KEY locally)";
    return { ok: false, skipped: true, reason: "ai_gateway_missing", error };
  }

  try {
    resetRunAudit();
    const agent = await ctx.getAgent();
    const turn = await agent.generate(buildRadarGoal(options), {
      resetConversation: false,
      channelKind: options.channelKind ?? "schedule",
    });
    void drainRunAudit();

    return {
      ok: true,
      skipped: false,
      agentText: turn.text,
    };
  } catch (err) {
    const message = String(err);
    return {
      ok: false,
      skipped: false,
      error: message,
    };
  }
}

export async function radarStatus(agentRoot: string): Promise<Record<string, unknown>> {
  const archive = await radarArchiveSummary(agentRoot, MCP_RADAR_INTENT);
  const last = archive.lastRun;

  return {
    gateway: gatewayConfigured(),
    tavily: tavilyConfigured(),
    lastRun: last,
    intent: MCP_RADAR_INTENT,
    observability: {
      archiveBackend: archive.archiveBackend,
      operatorRunsUrl: "/operator/runs",
      operatorSessionsUrl: "/operator/sessions",
      operatorArchivesUrl: "/operator/archives",
      totalArchivedRuns: archive.totalRuns,
      intentArchivedRuns: archive.intentRuns,
      runIds: last?.runIds ?? [],
      logicalSessionRef: last?.logicalSessionRef,
    },
  };
}
