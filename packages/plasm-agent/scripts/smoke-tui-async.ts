#!/usr/bin/env node
/**
 * Smoke: async session turn (wait:false) + live SSE step stream.
 */
import path from "node:path";
import { fileURLToPath } from "node:url";

import agentDefinition from "../agent/agent.js";
import { DevHttpSessionClient } from "../src/dev/client/http-session.js";
import { createDevServer } from "../src/dev/server.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const agentRoot = path.join(packageRoot, "agent");
const port = 4048 + Math.floor(Math.random() * 100);

async function main(): Promise<void> {
  const handle = await createDevServer({
    agentRoot,
    definition: agentDefinition,
    port,
    host: "127.0.0.1",
    telemetry: false,
  });

  try {
    const client = new DevHttpSessionClient(handle.url);
    const events: string[] = [];
    const { response } = await client.sendTurn("ping", null, {
      wait: false,
      onEvent: (ev) => events.push(ev.type),
    });
    if (!response.sessionId) throw new Error("missing sessionId");
    if (!events.includes("turn:start")) throw new Error(`missing turn:start: ${events.join(",")}`);
    if (!events.includes("turn:finish") && !events.includes("turn:error")) {
      throw new Error(`missing terminal event: ${events.join(",")}`);
    }
    console.log("OK: async session + SSE stream");
  } finally {
    await handle.close();
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
