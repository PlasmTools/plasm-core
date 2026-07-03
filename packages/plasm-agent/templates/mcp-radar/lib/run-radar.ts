import { type AuthoringContext } from "@plasm_lang/vercel-agent";

import { drainRunAudit, resetRunAudit } from "./run-audit.js";
import {
  gatewayConfigured,
  loadLastRun,
  saveLastRun,
  tavilyConfigured,
} from "./radar-state.js";

/** Stable intent — same string on every radar turn (`plasm_context` session reuse). */
export const MCP_RADAR_INTENT =
  "track MCP innovations from Hacker News and corroborate with Tavily web search";

export interface RadarRunOptions {
  force?: boolean;
  reset?: boolean;
}

export interface RadarRunResult {
  ok: boolean;
  skipped: boolean;
  reason?: string;
  agentText?: string;
  error?: string;
}

function buildRadarGoal(options: RadarRunOptions): string {
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
    await saveLastRun(ctx.agentRoot, {
      at: new Date().toISOString(),
      status: "error",
      message: error,
    });
    return { ok: false, skipped: true, reason: "ai_gateway_missing", error };
  }

  try {
    resetRunAudit();
    const agent = await ctx.getAgent();
    const turn = await agent.generate(buildRadarGoal(options), { resetConversation: false });
    const runAt = new Date().toISOString();
    const audit = drainRunAudit();

    await saveLastRun(ctx.agentRoot, {
      at: runAt,
      status: "ok",
      runIds: audit.runIds,
      logicalSessionRef: audit.logicalSessionRef,
    });

    return {
      ok: true,
      skipped: false,
      agentText: turn.text,
    };
  } catch (err) {
    const message = String(err);
    await saveLastRun(ctx.agentRoot, {
      at: new Date().toISOString(),
      status: "error",
      message,
    });
    return {
      ok: false,
      skipped: false,
      error: message,
    };
  }
}

export async function radarStatus(agentRoot: string): Promise<Record<string, unknown>> {
  const last = await loadLastRun(agentRoot);
  return {
    gateway: gatewayConfigured(),
    tavily: tavilyConfigured(),
    lastRun: last,
    intent: MCP_RADAR_INTENT,
    observability: {
      operatorRunsUrl: "/operator/runs",
      operatorSessionsUrl: "/operator/sessions",
      operatorArchivesUrl: "/operator/archives",
      runIds: last?.runIds ?? [],
      logicalSessionRef: last?.logicalSessionRef,
    },
  };
}
