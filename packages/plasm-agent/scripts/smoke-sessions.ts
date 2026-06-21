#!/usr/bin/env node
/**
 * Smoke: dev session API — continuationToken + SSE event replay.
 */
import path from "node:path";
import { fileURLToPath } from "node:url";

import agentDefinition from "../agent/agent.js";
import { createDevServer } from "../src/dev/server.js";

const packageRoot = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const agentRoot = path.join(packageRoot, "agent");
const port = 3948 + Math.floor(Math.random() * 100);

async function readSseEvents(url: string): Promise<string[]> {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`stream ${res.status}`);
  const text = await res.text();
  return text.split("\n\n").filter((block) => block.trim().length > 0);
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
    const infoRes = await fetch(`${handle.url}/plasm/v1/info`);
    const info = (await infoRes.json()) as { dev?: { routes?: { sessionStream?: string } } };
    if (!info.dev?.routes?.sessionStream) {
      throw new Error("info missing dev.routes.sessionStream");
    }

    const postRes = await fetch(`${handle.url}/plasm/v1/session`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ message: "ping" }),
    });
    const body = (await postRes.json()) as {
      sessionId?: string;
      continuationToken?: string;
      error?: string;
    };

    if (!body.sessionId || !body.continuationToken) {
      throw new Error(`session response missing ids: ${JSON.stringify(body)}`);
    }

    const blocks = await readSseEvents(
      `${handle.url}/plasm/v1/session/${body.sessionId}/stream`,
    );
    const hasStart = blocks.some((b) => b.includes("turn:start"));
    const hasTerminal = blocks.some(
      (b) => b.includes("turn:finish") || b.includes("turn:error"),
    );
    if (!hasStart || !hasTerminal) {
      throw new Error(`SSE missing events: ${blocks.join(" | ")}`);
    }

    const badContinue = await fetch(`${handle.url}/plasm/v1/session/${body.sessionId}`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ message: "nope", continuationToken: "wrong" }),
    });
    if (badContinue.status !== 404) {
      throw new Error(`expected 404 for bad continuationToken, got ${badContinue.status}`);
    }

    console.log("OK: session continuationToken + SSE replay");
  } finally {
    await handle.close();
  }
}

main().catch((err: unknown) => {
  console.error(err);
  process.exit(1);
});
