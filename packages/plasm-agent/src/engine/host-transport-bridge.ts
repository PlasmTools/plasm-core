/**
 * NAPI bridge: Rust threadsafe transport awaits a JavaScript Promise return.
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

/** Normalize sync or async host transport for `@plasm_lang/engine` `runPlanLive`. */
export function toNapiHostTransport(
  transport: HostTransportFn,
): (request: HostTransportRequest) => Promise<HostTransportResponse> {
  return async (request) => {
    const out = transport(request);
    const resolved = isPromise<HostTransportResponse>(out) ? await out : await Promise.resolve(out);
    if (resolved.status == null || resolved.body == null) {
      throw new Error("host transport response must include `status` and `body`");
    }
    return {
      status: resolved.status,
      body: resolved.body,
      nextUrl: resolved.nextUrl,
    };
  };
}
