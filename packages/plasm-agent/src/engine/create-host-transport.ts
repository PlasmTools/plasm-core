import { createDefaultHostTransport } from "./host-transport.js";
import { connectAuthOptionsForEntry } from "./connect-auth.js";
import type { HostTransportFn } from "./napi-binding.js";

/** Outbound HTTP for CGS stub live execute: Connect only when configured for the catalog. */
export function createStubHostTransport(entryId?: string): HostTransportFn {
  const useConnect = connectAuthOptionsForEntry(entryId)?.connector != null;
  return createDefaultHostTransport({ useConnect });
}

/** Production host transport: env bearer → Vercel Connect `getToken()` → fetch. */
export function createProductionHostTransport(): HostTransportFn {
  return createDefaultHostTransport({ useConnect: true });
}
