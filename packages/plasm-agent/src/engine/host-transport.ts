/**
 * Default outbound HTTP transport for local dev: `fetch` + bearer from env.
 */

import type { HostTransportFn, HostTransportRequest } from "./napi-binding.js";
import { resolveConnectBearer } from "./connect-auth.js";
import { plasmSpans } from "../telemetry/plasm-spans.js";

export type HostTransportOptions = {
  bearer?: string;
  fetchImpl?: typeof fetch;
  /** When true (default), resolve Vercel Connect tokens after env bearer misses. */
  useConnect?: boolean;
};

function bearerFromEnv(entryId?: string): string | undefined {
  const keys = [
    entryId ? `PLASM_${entryId.toUpperCase().replace(/[^A-Z0-9]/g, "_")}_BEARER` : undefined,
    "PLASM_BEARER",
    "PLASM_AUTH_BEARER",
  ].filter(Boolean) as string[];
  for (const key of keys) {
    const value = process.env[key]?.trim();
    if (value) return value;
  }
  return undefined;
}

export function createDefaultHostTransport(options?: HostTransportOptions): HostTransportFn {
  const fetchImpl = options?.fetchImpl ?? fetch;
  const bearerOverride = options?.bearer;
  const useConnect = options?.useConnect ?? true;

  return async (request: HostTransportRequest) => {
    const host = (() => {
      try {
        return new URL(request.url).host;
      } catch {
        return request.url;
      }
    })();

    return plasmSpans.transportHttp(
      {
        entryId: request.entryId,
        transportHost: host,
      },
      async (span) => {
        const headers = new Headers(request.headers ?? {});
        if (!headers.has("authorization")) {
          let bearer = bearerOverride ?? bearerFromEnv(request.entryId);
          if (!bearer && useConnect) {
            bearer = await resolveConnectBearer(request.entryId);
          }
          if (bearer) {
            headers.set("authorization", bearer.startsWith("Bearer ") ? bearer : `Bearer ${bearer}`);
          }
        }
        if (request.body != null && !headers.has("content-type")) {
          headers.set("content-type", "application/json; charset=utf-8");
        }

        const init: RequestInit = {
          method: request.method,
          headers,
        };
        if (request.body != null && request.method !== "GET" && request.method !== "HEAD") {
          init.body = request.body;
        }

        const response = await fetchImpl(request.url, init);
        const text = await response.text();
        const nextUrl = response.headers.get("link")?.match(/<([^>]+)>;\s*rel="?next"?/i)?.[1];

        span.setAttribute("plasm.transport.status", response.status);
        return {
          status: response.status,
          body: text,
          nextUrl: nextUrl ?? undefined,
        };
      },
    );
  };
}
