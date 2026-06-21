#!/usr/bin/env node
/**
 * Smoke: in-process prod PlasmApp handler (no listening dev server).
 */
import { createServer } from "node:http";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { createPlasmApp } from "../src/server/plasm-handler.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

async function main(): Promise<void> {
  const projectRoot =
    process.argv[2] ?? path.join(packageRoot, "../../../examples/mcp-radar-agent");
  const agentRoot = path.join(projectRoot, "agent");
  const agentUrl = pathToFileURL(path.join(agentRoot, "agent.ts")).href;
  const agentMod = (await import(agentUrl)) as { default: import("../src/define-agent.js").AgentDefinition };

  const app = await createPlasmApp({
    agentRoot,
    definition: agentMod.default,
    mode: "prod",
    sessions: false,
  });

  const server = createServer((req, res) => {
    void app.handleRequest(req, res);
  });

  await new Promise<void>((resolve, reject) => {
    server.listen(0, "127.0.0.1", () => resolve());
    server.once("error", reject);
  });

  const address = server.address();
  if (!address || typeof address === "string") {
    throw new Error("server address unavailable");
  }

  try {
    const statusRes = await fetch(
      `http://127.0.0.1:${address.port}/channel/mcp-radar/status`,
    );
    if (!statusRes.ok) {
      throw new Error(`status ${statusRes.status}: ${await statusRes.text()}`);
    }
    const status = (await statusRes.json()) as { intent?: string };
    if (!status.intent?.includes("MCP")) {
      throw new Error(`unexpected status: ${JSON.stringify(status)}`);
    }

    const infoRes = await fetch(`http://127.0.0.1:${address.port}/plasm/v1/info`);
    if (!infoRes.ok) {
      throw new Error(`info ${infoRes.status}`);
    }

    console.log("OK: prod PlasmApp handler (status + info)");
  } finally {
    await new Promise<void>((resolve, reject) => {
      server.close((err) => (err ? reject(err) : resolve()));
    });
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
