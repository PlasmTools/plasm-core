import assert from "node:assert/strict";
import { createDefaultHostTransport } from "../src/engine/host-transport.js";

const calls: RequestInit[] = [];
const transport = createDefaultHostTransport({
  useConnect: false,
  bearer: "synthetic-token",
  fetchImpl: async (_url, init) => {
    assert.ok(init);
    calls.push(init);
    return new Response("{}", { status: 200 });
  },
});

for (const rejectRedirects of [true, false]) {
  await transport({
    method: "GET",
    url: "https://example.test/records/a",
    headers: {},
    rejectRedirects,
    requireHostAuth: true,
  });
}
assert.equal(calls[0].redirect, "error");
assert.equal(calls[1].redirect, "follow");
assert.equal(new Headers(calls[0].headers).get("authorization"), "Bearer synthetic-token");
const missing = createDefaultHostTransport({
  useConnect: false,
  bearer: "",
  fetchImpl: async () => { throw new Error("must not dispatch"); },
});
await assert.rejects(missing({
  method: "GET",
  url: "https://example.test/records/a",
  headers: {},
  rejectRedirects: true,
  requireHostAuth: true,
}), /Scoped host authentication is not configured/);
console.log("Scoped transport host injection and redirect policy passed (no network).");
