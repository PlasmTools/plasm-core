import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";

export interface LastRunMeta {
  at: string;
  status: "ok" | "skipped" | "error";
  message?: string;
  runIds?: string[];
  logicalSessionRef?: string;
}

function researchDir(agentRoot: string): string {
  return path.join(agentRoot, ".plasm", "research");
}

function lastRunPath(agentRoot: string): string {
  return path.join(researchDir(agentRoot), "last-run.json");
}

export async function loadLastRun(agentRoot: string): Promise<LastRunMeta | null> {
  try {
    const raw = await readFile(lastRunPath(agentRoot), "utf8");
    return JSON.parse(raw) as LastRunMeta;
  } catch {
    return null;
  }
}

export async function saveLastRun(agentRoot: string, meta: LastRunMeta): Promise<void> {
  await mkdir(researchDir(agentRoot), { recursive: true });
  await writeFile(lastRunPath(agentRoot), `${JSON.stringify(meta, null, 2)}\n`, "utf8");
}

/** Host infra — not a substitute for Plasm catalog calls. */
export function gatewayConfigured(): boolean {
  if (
    process.env.AI_GATEWAY_API_KEY?.trim() ||
    process.env.AI_API_GATEWAY_KEY?.trim() ||
    process.env.AI_GATEWAY_KEY?.trim()
  ) {
    return true;
  }
  return (
    process.env.VERCEL === "1" ||
    Boolean(process.env.VERCEL_DEPLOYMENT_ID?.trim()) ||
    Boolean(process.env.VERCEL_ENV?.trim())
  );
}

/** Outbound Tavily auth present on host — agent still calls Tavily via Plasm. */
export function tavilyConfigured(): boolean {
  return Boolean(process.env.TAVILY_API_TOKEN?.trim());
}
