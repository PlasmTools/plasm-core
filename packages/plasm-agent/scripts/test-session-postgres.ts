#!/usr/bin/env node
import assert from "node:assert/strict";
import pg from "pg";
import { PostgresStateAdapter } from "../src/state/postgres-state-adapter.js";
import { sessionStorageKey } from "../src/runtime/session-contract.js";
import { checkSessionExtension } from "./test-session-extension.js";
import { identityOf, sessionCases } from "./test-session-contract.js";

const connectionString = process.env.PLASM_TEST_POSTGRES_URL;
assert.ok(connectionString, "PLASM_TEST_POSTGRES_URL is required; this test must exercise PostgreSQL");
const db = new pg.Client({ connectionString });
await db.connect();
try {
  // Connection-local shadow table; no durable schema, application rows, or catalogs change.
  await db.query(`CREATE TEMP TABLE plasm_agent_state (
    tenant_id TEXT NOT NULL, kind TEXT NOT NULL, state_key TEXT NOT NULL,
    payload JSONB NOT NULL, updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, kind, state_key)
  )`);
  await assert.rejects(
    () => db.query("SELECT to_tsvector('english', $1::text)", ["read records\0l_storage_address"]),
    (error: unknown) => error instanceof Error && error.message.includes('0x00'),
    "confirm the original PostgreSQL boundary failure",
  );
  await checkSessionExtension(async (intent) => {
    await db.query("SELECT to_tsvector('english', $1::text)", [intent]);
  });
  const seed = Number(process.env.PLASM_PROPERTY_SEED ?? 0x51a17e);
  let count = 0;
  for (const state of sessionCases(128, seed)) {
    try {
      const store = new PostgresStateAdapter(state.tenantScope, db);
      await store.put(state);
      const reopened = new PostgresStateAdapter(state.tenantScope, db);
      assert.deepEqual(await reopened.get(state.logicalSessionRef), state);
      assert.deepEqual(await reopened.listSessions(), [state]);
      const text = await db.query("SELECT $1::text AS intent, to_tsvector('english', $1::text) AS search", [state.intent]);
      assert.equal(text.rows[0].intent, state.intent);
      const stranger = new PostgresStateAdapter(`${state.tenantScope}:other`, db);
      assert.equal(await stranger.get(state.logicalSessionRef), null);
      await assert.rejects(() => stranger.put(state), /identity/);
      const key = sessionStorageKey(identityOf(state));
      await db.query("UPDATE plasm_agent_state SET payload = jsonb_set(payload, '{tenantScope}', to_jsonb('wrong-tenant'::text)) WHERE tenant_id=$1 AND state_key=$2", [state.tenantScope, key]);
      await assert.rejects(() => reopened.get(state.logicalSessionRef), /identity/);
      await assert.rejects(() => reopened.listSessions(), /identity/);
      await db.query("DELETE FROM plasm_agent_state WHERE tenant_id=$1 AND state_key=$2", [state.tenantScope, key]);
    } catch (error) { throw new Error(`PostgreSQL session property failed: seed=${seed}, case=${count}`, { cause: error }); }
    count++;
  }
  console.log(`session-postgres: ${count} generated JSONB/text/identity properties; new -> persist -> extend accepted by text search; seed=${seed}`);
} finally { await db.end(); }
