import {
  type AuthoringContext,
} from "@plasm_lang/vercel-agent";

import {
  addSeenIds,
  appendProofRun,
  gatewayConfigured,
  loadLastRun,
  loadSeenState,
  saveLastRun,
  saveSeenState,
  tavilyConfigured,
} from "./proof-store.js";

export const MCP_RADAR_INTENT =
  "track MCP innovations from Hacker News and corroborate with Tavily web search";

export const MCP_SEARCH_QUERY = "MCP OR Model Context Protocol";

export interface HnStoryCandidate {
  id: string;
  title?: string;
  url?: string;
}

export interface RadarRunOptions {
  force?: boolean;
}

export interface RadarRunResult {
  ok: boolean;
  skipped: boolean;
  reason?: string;
  candidates: HnStoryCandidate[];
  newCandidates: HnStoryCandidate[];
  agentText?: string;
  error?: string;
}

type HnStubModule = {
  item_search?: (
    input: { query: string; tags?: string; per_page?: number },
    options?: { transport?: unknown },
  ) => Promise<Array<{ id?: string | number; title?: string; url?: string }>>;
};

export async function preflightHnMcpStories(ctx: AuthoringContext): Promise<HnStoryCandidate[]> {
  try {
    const mod = (await ctx.importStub("hackernews")) as HnStubModule;
    if (typeof mod.item_search !== "function") return [];
    const rows = await mod.item_search(
      { query: MCP_SEARCH_QUERY, tags: "story", per_page: 10 },
    );
    return rows
      .map((row) => ({
        id: String(row.id ?? ""),
        title: row.title,
        url: row.url,
      }))
      .filter((row) => row.id.length > 0);
  } catch (err) {
    console.warn("[mcp-radar] preflight HN search failed:", err);
    return [];
  }
}

function filterNewCandidates(
  candidates: HnStoryCandidate[],
  seenIds: Set<string>,
  force: boolean,
): HnStoryCandidate[] {
  if (force) return candidates;
  return candidates.filter((c) => !seenIds.has(c.id));
}

function buildAgentGoal(newCandidates: HnStoryCandidate[], tavily: boolean): string {
  const lines = newCandidates.map(
    (c) => `- id=${c.id} title=${JSON.stringify(c.title ?? "")} url=${JSON.stringify(c.url ?? "")}`,
  );
  const tavilyNote = tavily
    ? "Tavily is configured — corroborate each story with web_search."
    : "Tavily is NOT configured — HN-only synthesis; use Confidence: low.";
  return [
    "Produce MCP proof entries for these NEW Hacker News story candidates:",
    ...lines,
    "",
    tavilyNote,
    "",
    "Use plasm_context with stable intent for hackernews + tavily, then plan and run programs.",
    "Output markdown proof blocks per the mcp-proof-format skill (### headers).",
    "Do not skip candidates unless they are clearly unrelated to MCP.",
  ].join("\n");
}

export async function runRadar(
  ctx: AuthoringContext,
  options: RadarRunOptions = {},
): Promise<RadarRunResult> {
  const force = options.force === true;
  const seen = await loadSeenState(ctx.agentRoot);
  const seenSet = new Set(seen.itemIds);

  const candidates = await preflightHnMcpStories(ctx);
  const newCandidates = filterNewCandidates(candidates, seenSet, force);

  if (!newCandidates.length && !force) {
    const result: RadarRunResult = {
      ok: true,
      skipped: true,
      reason: candidates.length ? "no_new_stories" : "no_hn_hits",
      candidates,
      newCandidates: [],
    };
    await saveSeenState(ctx.agentRoot, {
      ...seen,
      lastRunAt: new Date().toISOString(),
      lastRunStatus: "skipped",
      lastNewCount: 0,
    });
    await saveLastRun(ctx.agentRoot, {
      at: new Date().toISOString(),
      status: "skipped",
      newItems: 0,
      message: result.reason,
    });
    return result;
  }

  if (!gatewayConfigured()) {
    const result: RadarRunResult = {
      ok: false,
      skipped: true,
      reason: "ai_gateway_missing",
      candidates,
      newCandidates,
      error: "AI_GATEWAY_API_KEY is required for agent synthesis",
    };
    await saveLastRun(ctx.agentRoot, {
      at: new Date().toISOString(),
      status: "error",
      newItems: 0,
      message: result.error,
    });
    return result;
  }

  const toProcess = newCandidates.length ? newCandidates : candidates;
  const goal = buildAgentGoal(toProcess, tavilyConfigured());

  try {
    const agent = await ctx.getAgent();
    const turn = await agent.generate(goal, { resetConversation: false });
    const runAt = new Date().toISOString();

    if (turn.text.trim()) {
      await appendProofRun(ctx.agentRoot, { runAt, body: turn.text });
    }

    const ids = toProcess.map((c) => c.id);
    await addSeenIds(ctx.agentRoot, ids);
    await saveSeenState(ctx.agentRoot, {
      ...(await loadSeenState(ctx.agentRoot)),
      lastRunAt: runAt,
      lastRunStatus: "ok",
      lastNewCount: toProcess.length,
    });
    await saveLastRun(ctx.agentRoot, {
      at: runAt,
      status: "ok",
      newItems: toProcess.length,
    });

    return {
      ok: true,
      skipped: false,
      candidates,
      newCandidates: toProcess,
      agentText: turn.text,
    };
  } catch (err) {
    const message = String(err);
    await saveSeenState(ctx.agentRoot, {
      ...(await loadSeenState(ctx.agentRoot)),
      lastRunAt: new Date().toISOString(),
      lastRunStatus: "error",
    });
    await saveLastRun(ctx.agentRoot, {
      at: new Date().toISOString(),
      status: "error",
      newItems: 0,
      message,
    });
    return {
      ok: false,
      skipped: false,
      candidates,
      newCandidates: toProcess,
      error: message,
    };
  }
}

export async function radarStatus(agentRoot: string): Promise<Record<string, unknown>> {
  const seen = await loadSeenState(agentRoot);
  const last = await loadLastRun(agentRoot);
  return {
    gateway: gatewayConfigured(),
    tavily: tavilyConfigured(),
    seenCount: seen.itemIds.length,
    lastRun: last,
    intent: MCP_RADAR_INTENT,
  };
}
