import type { IncomingMessage, ServerResponse } from "node:http";
import { z } from "zod";

import type { PlasmAgent, AgentTurnResult } from "../runtime/plasm-agent.js";
import { readJsonBody, sendJson } from "./http.js";
import {
  DevSessionStore,
  formatSseEvent,
  type DevSessionRecord,
} from "./dev-session.js";

const newSessionBody = z.object({
  message: z.string().trim().min(1),
  wait: z.boolean().optional(),
});

const continueSessionBody = z.object({
  message: z.string().trim().min(1),
  continuationToken: z.string().trim().min(1),
  wait: z.boolean().optional(),
});

export interface SessionRouteContext {
  sessionStore: DevSessionStore;
  getAgent: () => Promise<PlasmAgent>;
}

function sessionJson(
  session: DevSessionRecord,
  extra: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    sessionId: session.id,
    continuationToken: session.continuationToken,
    status: session.status,
    ...extra,
  };
}

async function respondSessionTurn(
  res: ServerResponse,
  session: DevSessionRecord,
  run: () => Promise<AgentTurnResult>,
): Promise<void> {
  try {
    const result = await run();
    sendJson(res, 200, sessionJson(session, {
      text: result.text,
      usage: result.usage,
      steps: result.steps.length,
    }));
  } catch (err) {
    sendJson(res, 500, sessionJson(session, {
      error: "turn_failed",
      message: String(err),
    }));
  }
}

function startAsyncTurn(
  session: DevSessionRecord,
  message: string,
  agent: PlasmAgent,
  sessionStore: DevSessionStore,
): void {
  void sessionStore.runTurn(session, message, agent).catch((err) => {
    console.error("[plasm:dev] async turn failed:", err);
  });
}

function handleSessionStream(
  res: ServerResponse,
  session: DevSessionRecord,
  sessionStore: DevSessionStore,
): void {
  res.statusCode = 200;
  res.setHeader("content-type", "text/event-stream; charset=utf-8");
  res.setHeader("cache-control", "no-cache");
  res.setHeader("connection", "keep-alive");

  for (const event of session.events) {
    res.write(formatSseEvent(event));
  }

  const terminal = session.status === "idle" || session.status === "error";
  if (terminal) {
    res.end();
    return;
  }

  let unsubscribe = () => {};
  unsubscribe = sessionStore.subscribe(session.id, (event) => {
    res.write(formatSseEvent(event));
    if (event.type === "turn:finish" || event.type === "turn:error") {
      unsubscribe();
      res.end();
    }
  });
  res.on("close", unsubscribe);
}

/**
 * Session routes. `wait: false` returns immediately (202) and runs the turn in the background;
 * clients attach to GET /stream for step events. Default `wait: true` blocks until the turn ends.
 */
export async function tryHandleSessionRoutes(
  req: IncomingMessage,
  res: ServerResponse,
  url: URL,
  ctx: SessionRouteContext,
): Promise<boolean> {
  const method = req.method ?? "GET";

  if (method === "POST" && url.pathname === "/plasm/v1/session") {
    const parsed = newSessionBody.safeParse(await readJsonBody(req));
    if (!parsed.success) {
      sendJson(res, 400, { error: "message_required" });
      return true;
    }

    const session = ctx.sessionStore.create();
    const agent = await ctx.getAgent();
    if (parsed.data.wait === false) {
      startAsyncTurn(session, parsed.data.message, agent, ctx.sessionStore);
      sendJson(res, 202, sessionJson(session));
      return true;
    }

    await respondSessionTurn(res, session, () =>
      ctx.sessionStore.runTurn(session, parsed.data.message, agent),
    );
    return true;
  }

  const streamMatch = url.pathname.match(/^\/plasm\/v1\/session\/([^/]+)\/stream$/);
  if (method === "GET" && streamMatch) {
    const sessionId = streamMatch[1]!;
    const session = ctx.sessionStore.get(sessionId);
    if (!session) {
      sendJson(res, 404, { error: "session_not_found", sessionId });
      return true;
    }
    handleSessionStream(res, session, ctx.sessionStore);
    return true;
  }

  const continueMatch = url.pathname.match(/^\/plasm\/v1\/session\/([^/]+)$/);
  if (method === "POST" && continueMatch) {
    const sessionId = continueMatch[1]!;
    const raw = await readJsonBody(req);
    const tokenParsed = z.object({ continuationToken: z.string().trim().min(1) }).safeParse(raw);
    if (!tokenParsed.success) {
      sendJson(res, 400, { error: "continuation_token_required" });
      return true;
    }
    const messageParsed = continueSessionBody.safeParse(raw);
    if (!messageParsed.success) {
      sendJson(res, 400, { error: "message_required" });
      return true;
    }

    const session = ctx.sessionStore.validateContinuation(
      sessionId,
      messageParsed.data.continuationToken,
    );
    if (!session) {
      sendJson(res, 404, { error: "session_not_found", sessionId });
      return true;
    }

    const agent = await ctx.getAgent();
    if (messageParsed.data.wait === false) {
      startAsyncTurn(session, messageParsed.data.message, agent, ctx.sessionStore);
      sendJson(res, 202, sessionJson(session));
      return true;
    }

    await respondSessionTurn(res, session, () =>
      ctx.sessionStore.runTurn(session, messageParsed.data.message, agent),
    );
    return true;
  }

  return false;
}
