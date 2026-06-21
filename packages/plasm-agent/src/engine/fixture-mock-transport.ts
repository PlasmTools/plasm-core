/**
 * Dev mock HTTP for fixture catalogs (`http://example.test` backends).
 * Enables full `plasm_run` smoke without a live API.
 */

import type { HostTransportFn } from "./napi-binding.js";

/** Mock product list/get responses for `execute_tiny` fixture paths. */
export function createFixtureMockTransport(): HostTransportFn {
  return async (request) => {
    const path = (() => {
      try {
        return new URL(request.url).pathname;
      } catch {
        return request.url;
      }
    })();

    if (path.includes("/products/") && path !== "/products" && !path.includes("/search")) {
      return {
        status: 200,
        body: JSON.stringify({ id: "p1", name: "Widget", category_id: "c1" }),
      };
    }
    if (path.includes("/products")) {
      return {
        status: 200,
        body: JSON.stringify([{ id: "p1", name: "Widget", category_id: "c1" }]),
      };
    }
    if (path.includes("/categories")) {
      return {
        status: 200,
        body: JSON.stringify({ id: "c1", name: "Gadgets" }),
      };
    }

    return {
      status: 404,
      body: JSON.stringify({ error: `fixture mock: unhandled path ${path}` }),
    };
  };
}

export function fixtureMockTransportEnabled(): boolean {
  const flag = process.env.PLASM_AGENT_MOCK_HTTP?.trim();
  if (flag === "1" || flag === "true") return true;
  if (flag === "0" || flag === "false") return false;
  // Default on for local dev when no outbound bearer is configured.
  return !(
    process.env.PLASM_BEARER?.trim() ||
    process.env.PLASM_AUTH_BEARER?.trim()
  );
}
