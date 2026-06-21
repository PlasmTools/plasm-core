import { createDefaultHostTransport } from "./host-transport.js";
import type { HostTransportFn } from "./napi-binding.js";

/** Production host transport: env bearer → Vercel Connect `getToken()` → fetch. */
export function createProductionHostTransport(): HostTransportFn {
  return createDefaultHostTransport({ useConnect: true });
}
