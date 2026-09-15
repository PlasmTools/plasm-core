/**
 * NAPI bridge: Rust threadsafe transport awaits a JavaScript Promise return.
 *
 * The TSFN callback must resolve a `{status, body}` value. A thrown JS error
 * (missing bearer, fetch failure, author transport bug) can abort the isolate
 * instead of rejecting the Rust future — live `plasm_run` then kills the agent
 * process before wrap logging. Always settle with a status.
 */

import type { HostTransportFn, HostTransportRequest, HostTransportResponse } from "./napi-binding.js";

function isPromise<T>(value: unknown): value is Promise<T> {
  return (
    value != null &&
    typeof value === "object" &&
    "then" in value &&
    typeof (value as Promise<T>).then === "function"
  );
}

/** Non-2xx so Rust `parse_response` becomes a typed RequestError, not a panic. */
export function hostTransportFault(message: string): HostTransportResponse {
  return {
    status: 599,
    body: JSON.stringify({ error: message }),
  };
}

/** Normalize sync or async host transport for `@plasm_lang/engine` `runPlanLive`. */
export function toNapiHostTransport(
  transport: HostTransportFn,
): (request: HostTransportRequest) => Promise<HostTransportResponse> {
  return async (request) => {
    try {
      const out = transport(request);
      const resolved = isPromise<HostTransportResponse>(out) ? await out : await Promise.resolve(out);
      if (resolved.status == null || resolved.body == null) {
        return hostTransportFault("host transport response must include `status` and `body`");
      }
      return {
        status: resolved.status,
        body: resolved.body,
        nextUrl: resolved.nextUrl,
      };
    } catch (err) {
      return hostTransportFault(err instanceof Error ? err.message : String(err));
    }
  };
}
