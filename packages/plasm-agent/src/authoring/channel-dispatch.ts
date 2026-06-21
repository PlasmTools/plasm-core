import type { IncomingMessage, ServerResponse } from "node:http";

import type { AuthoringContext } from "./context.js";
import type { LoadedChannel } from "./slot-loader.js";

function sendJson(res: ServerResponse, status: number, payload: unknown): void {
  const body = JSON.stringify(payload, null, 2);
  res.statusCode = status;
  res.setHeader("content-type", "application/json; charset=utf-8");
  res.end(body);
}

export function tryHandleChannelRoute(
  req: IncomingMessage,
  res: ServerResponse,
  channels: LoadedChannel[],
  ctx: AuthoringContext,
): boolean {
  const method = (req.method ?? "GET").toUpperCase();
  const pathname = new URL(req.url ?? "/", "http://localhost").pathname;

  for (const channel of channels) {
    for (const route of channel.definition.routes) {
      if (route.method !== method || route.path !== pathname) continue;
      void Promise.resolve(route.handler(req, res, ctx)).catch((err: unknown) => {
        if (!res.writableEnded) {
          sendJson(res, 500, { error: "channel_handler_error", message: String(err) });
        }
      });
      return true;
    }
  }

  return false;
}

export function listChannelRoutes(channels: LoadedChannel[]): Array<{ method: string; path: string }> {
  return channels.flatMap((channel) =>
    channel.definition.routes.map((route) => ({
      method: route.method,
      path: route.path,
    })),
  );
}
