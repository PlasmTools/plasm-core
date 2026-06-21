import { access, readFile } from "node:fs/promises";
import path from "node:path";

export interface ResolvedAgentProject {
  /** Directory containing `agent/agent.ts`. */
  projectRoot: string;
  agentRoot: string;
}

async function exists(p: string): Promise<boolean> {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

/** Walk up from `startDir` to find `agent/agent.ts`. */
export async function resolveAgentProject(
  startDir: string = process.cwd(),
): Promise<ResolvedAgentProject | null> {
  let current = path.resolve(startDir);
  for (let depth = 0; depth < 12; depth += 1) {
    const agentRoot = path.join(current, "agent");
    const agentTs = path.join(agentRoot, "agent.ts");
    if (await exists(agentTs)) {
      return { projectRoot: current, agentRoot };
    }
    const parent = path.dirname(current);
    if (parent === current) break;
    current = parent;
  }
  return null;
}

export async function requireAgentProject(
  startDir?: string,
): Promise<ResolvedAgentProject> {
  const resolved = await resolveAgentProject(startDir);
  if (!resolved) {
    throw new Error(
      "No agent project found. Run from a directory with agent/agent.ts or use `plasm-agent init`.",
    );
  }
  return resolved;
}

export async function readPackageName(projectRoot: string): Promise<string | undefined> {
  try {
    const raw = await readFile(path.join(projectRoot, "package.json"), "utf8");
    const parsed = JSON.parse(raw) as { name?: string };
    return parsed.name;
  } catch {
    return undefined;
  }
}
