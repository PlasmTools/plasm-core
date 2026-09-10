#!/usr/bin/env node
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { intentKey, LocalSessionStore } from "../src/session-state.js";

const long = `${"Read owe_list.csv, ".repeat(40)}and federate Venmo plus Splitwise.`;
assert.ok(long.length > 400, "fixture must exceed typical filename blow-up");
const key = intentKey(long);
assert.equal(key.length, 64);
assert.match(key, /^[0-9a-f]{64}$/);
assert.notEqual(intentKey(long), intentKey(`${long}x`));

const root = mkdtempSync(path.join(tmpdir(), "plasm-session-key-"));
const store = new LocalSessionStore(root);
const state = {
  intent: long,
  logicalSessionRef: "l_test",
  logicalSessionId: "sid",
  tenantScope: "local",
  seeds: [],
  teachingTsv: "e1\tFile",
  waves: [],
  planCommits: [],
  updatedAt: new Date().toISOString(),
};
await store.put(state);
const got = await store.get(long);
assert.equal(got?.logicalSessionRef, "l_test");
assert.equal(got?.intent, long);
console.log("session-intent-key: ok");
