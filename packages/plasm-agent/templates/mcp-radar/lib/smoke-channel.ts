#!/usr/bin/env node
/**
 * Smoke: MCP Radar channel routes (status, proof link, run with gateway check).
 */
import { createPlasmDevServer } from "@plasm_lang/vercel-agent/dev";

import { gatewayConfigured } from "../lib/radar-state.js";

async function main(): Promise<void> {
  const handle = await createPlasmDevServer({ port: 0 });
  try {
    const statusRes = await fetch(`${handle.url}/channel/mcp-radar/status`);
    if (!statusRes.ok) throw new Error(`status ${statusRes.status}`);
    const status = (await statusRes.json()) as { gateway?: boolean; intent?: string };
    if (status.intent == null) throw new Error("status missing intent");

    const proofRes = await fetch(`${handle.url}/channel/mcp-radar/proof`);
    if (proofRes.status === 404) {
      console.log("OK: proof route (not configured locally)");
    } else if (!proofRes.ok) {
      throw new Error(`proof ${proofRes.status}`);
    } else {
      const proof = (await proofRes.json()) as { editor?: string };
      if (proof.editor !== "proof") throw new Error("proof response missing editor=proof");
    }

    if (gatewayConfigured()) {
      console.log("OK: gateway configured for agent runs");
    } else {
      console.log("SKIP: gateway not configured locally");
    }

    console.log(`OK: mcp-radar channel smoke (${handle.url})`);
  } finally {
    await handle.close();
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
