#!/usr/bin/env node
/**
 * Smoke: MCP Radar channel routes (status, proof, run with gateway check).
 */
import path from "node:path";
import { fileURLToPath } from "node:url";

import agentDefinition from "../agent/agent.js";
import { createDevServer } from "@plasm_lang/vercel-agent";
import { gatewayConfigured } from "../lib/proof-store.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const agentRoot = path.join(packageRoot, "agent");
const port = 4148 + Math.floor(Math.random() * 100);

async function main(): Promise<void> {
  const handle = await createDevServer({
    agentRoot,
    definition: agentDefinition,
    port,
    host: "127.0.0.1",
    telemetry: false,
  });

  try {
    const statusRes = await fetch(`${handle.url}/channel/mcp-radar/status`);
    if (!statusRes.ok) {
      const body = await statusRes.text();
      throw new Error(`status ${statusRes.status}: ${body}`);
    }
    const status = (await statusRes.json()) as { intent?: string };
    if (!status.intent?.includes("MCP")) throw new Error("unexpected status payload");

    const proofRes = await fetch(`${handle.url}/channel/mcp-radar/proof`);
    if (!proofRes.ok) throw new Error(`proof ${proofRes.status}`);
    const proof = await proofRes.text();
    if (!proof.includes("MCP Innovations Proof Log")) {
      throw new Error("proof markdown missing header");
    }

    const runRes = await fetch(`${handle.url}/channel/mcp-radar/run`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({}),
    });
    const runBody = (await runRes.json()) as { ok?: boolean; skipped?: boolean; reason?: string };
    if (!gatewayConfigured()) {
      if (runBody.reason !== "ai_gateway_missing") {
        throw new Error(`expected ai_gateway_missing without gateway, got ${JSON.stringify(runBody)}`);
      }
      console.log("OK: channel routes (run skipped without AI_GATEWAY_API_KEY)");
    } else {
      if (!runBody.ok && !runBody.skipped) {
        throw new Error(`run failed: ${JSON.stringify(runBody)}`);
      }
      console.log("OK: channel routes including run");
    }
  } finally {
    await handle.close();
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
