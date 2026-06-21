#!/usr/bin/env node
/**
 * Smoke: dev server hot-reload refreshes loadedSlots after skills/ change.
 */
import { mkdir, writeFile, rm } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import agentDefinition from "../agent/agent.js";
import { createDevServer } from "../src/dev/server.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const agentRoot = path.join(packageRoot, "agent");
const port = 3848 + Math.floor(Math.random() * 100);

interface InfoPayload {
  loadedSlots?: { skills?: Array<{ name: string }> };
}

async function fetchInfo(url: string): Promise<InfoPayload> {
  const res = await fetch(`${url}/plasm/v1/info`);
  if (!res.ok) throw new Error(`info ${res.status}`);
  return res.json() as Promise<InfoPayload>;
}

async function main(): Promise<void> {
  const handle = await createDevServer({
    agentRoot,
    definition: agentDefinition,
    port,
    host: "127.0.0.1",
    telemetry: false,
  });

  try {
    const before = await fetchInfo(handle.url);
    const skillCountBefore = before.loadedSlots?.skills?.length ?? 0;

    const tmpSkillDir = path.join(agentRoot, "skills");
    await mkdir(tmpSkillDir, { recursive: true });
    const tmpSkill = path.join(tmpSkillDir, "hot-reload-smoke.md");
    await writeFile(tmpSkill, "# Hot reload smoke\n\nTemporary skill for smoke test.\n", "utf8");

    await new Promise((r) => setTimeout(r, 400));
    await handle.reload();

    const after = await fetchInfo(handle.url);
    const names = (after.loadedSlots?.skills ?? []).map((s) => s.name);
    if (!names.includes("hot-reload-smoke")) {
      throw new Error(`hot-reload-smoke skill not in loadedSlots: ${names.join(", ")}`);
    }
    if ((after.loadedSlots?.skills?.length ?? 0) <= skillCountBefore) {
      throw new Error("expected skill count to increase after hot reload");
    }

    await rm(tmpSkill, { force: true });
    console.log("OK: hot reload refreshed loadedSlots");
  } finally {
    await handle.close();
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
