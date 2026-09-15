#!/usr/bin/env node
/**
 * Compose: JS host-transport faults settle as `{status, body}`.
 * No AppWorld / dest liturgy — the NAPI TSFN callback must not throw.
 */
import assert from "node:assert/strict";

import { createDefaultHostTransport } from "../src/engine/host-transport.js";
import { hostTransportFault, toNapiHostTransport } from "../src/engine/host-transport-bridge.js";

const missingAuth = await createDefaultHostTransport({
  useConnect: false,
  allowGlobalBearer: false,
  fetchImpl: async () => {
    throw new Error("must not dispatch");
  },
})({
  method: "GET",
  url: "https://example.test/records/a",
  headers: {},
  rejectRedirects: true,
  requireHostAuth: true,
});
assert.equal(missingAuth.status, 401);
assert.match(missingAuth.body, /Scoped host authentication is not configured/);

const bridged = toNapiHostTransport(async () => {
  throw new Error("synthetic transport fault");
});
const settled = await bridged({
  method: "GET",
  url: "https://example.test/boom",
  headers: {},
  rejectRedirects: false,
  requireHostAuth: false,
});
assert.deepEqual(settled, hostTransportFault("synthetic transport fault"));

const fetchBoom = await createDefaultHostTransport({
  useConnect: false,
  allowGlobalBearer: false,
  fetchImpl: async () => {
    throw new Error("connect ECONNREFUSED");
  },
})({
  method: "GET",
  url: "http://127.0.0.1:9/",
  headers: {},
  rejectRedirects: false,
  requireHostAuth: false,
});
assert.equal(fetchBoom.status, 599);
assert.match(fetchBoom.body, /ECONNREFUSED/);

console.log("OK: live transport faults stay in-band; process survived.");
