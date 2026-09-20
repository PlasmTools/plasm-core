#!/usr/bin/env node
import assert from "node:assert/strict";
import { mkdtemp, rm, readFile, writeFile, readdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { decodeSessionState, encodeSessionState, LocalSessionStore, SessionManager, type AgentSessionState } from "../src/session-state.js";
import { logicalSessionRefSchema, workflowIntentSchema, deriveIntent, intentProvenanceSchema, sessionStorageKey, sessionTenantKey, type SessionIdentity } from "../src/runtime/session-contract.js";
import { formatLogicalSessionWireRef } from "../src/runtime/logical-session.js";

// Reproducible generated properties without adding a property-test dependency.
// Failure output includes seed and case, so every generated input is replayable.
export function* sessionCases(count: number, seed = 0x51a17e): Generator<AgentSessionState> {
  let state = seed >>> 0;
  const next = () => { state ^= state << 13; state ^= state >>> 17; state ^= state << 5; return state >>> 0; };
  const text = () => {
    let result = "";
    const length = next() % 80;
    for (let i = 0; i < length; i++) {
      const scalar = next() % 0x110000;
      if (scalar !== 0 && !(scalar >= 0xd800 && scalar <= 0xdfff)) result += String.fromCodePoint(scalar);
    }
    return result;
  };
  const edges = ["\n\r\t\u0001", "\\0:/:\"[]{}", "e\u0301é", "漢字🦾", "x".repeat(4096), "l_0123456789012345678901"];
  for (let i = 0; i < count; i++) {
    const bytes = Buffer.alloc(16);
    for (let j = 0; j < 16; j++) bytes[j] = next() % 256;
    bytes[6] = (bytes[6]! & 15) | 64;
    bytes[8] = (bytes[8]! & 63) | 128;
    const hex = bytes.toString("hex");
    const intent = workflowIntentSchema.parse(`Read ${edges[i % edges.length]} ${text()}`);
    let intentProvenance = deriveIntent(undefined, intent);
    for (let turn = next() % 8; turn > 0; turn--) intentProvenance = deriveIntent(intentProvenance, workflowIntentSchema.parse(`Resolve ${text()}`));
    yield {
      schemaVersion: 2,
      intentProvenance,
      intent,
      logicalSessionRef: logicalSessionRefSchema.parse(formatLogicalSessionWireRef(bytes)),
      logicalSessionId: `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`,
      tenantScope: `tenant:${text()}`,
      seeds: [{ api: "matrix", entity: text() }], teachingTsv: text(),
      waves: [{ entryId: "matrix", entities: [text()], tsv: text(), at: "2026-09-20T00:00:00.000Z" }],
      planCommits: [{ ref: "pc0", program: text(), at: "2026-09-20T00:00:00.000Z", writeCount: i % 3 }],
      routingClosures: [{
        business: [{ catalog: "matrix", capability: "read" }], input_sources: [], prerequisites: [],
        acquisitions: [{ id: "a0", provider_catalog: "matrix", provider: "Record", capability: { catalog: "matrix", capability: "read" }, arguments: { query: text(), nested: [true, null, i, { value: text() }] } }],
        edges: [],
      }],
      symbolRegistry: { bindings: [{ symbol: "e1", kind: "entity", entryId: "matrix", wire: text() }], nextEntity: 2, nextMethod: 1, nextParam: 1, nextRelation: 1 },
      updatedAt: "2026-09-20T00:00:00.000Z",
    };
  }
}

export function identityOf(state: AgentSessionState): SessionIdentity {
  return { tenantScope: state.tenantScope, logicalSessionRef: state.logicalSessionRef };
}

async function main(): Promise<void> {
  const seed = Number(process.env.PLASM_PROPERTY_SEED ?? 0x51a17e);
  const count = 1024;
  const root = await mkdtemp(path.join(tmpdir(), "plasm-session-properties-"));
  let index = 0;
  try {
    for (const state of sessionCases(count, seed)) {
      try {
        const identity = identityOf(state);
        const encoded = encodeSessionState(state, identity);
        const decoded = decodeSessionState(JSON.parse(encoded), identity);
        assert.deepEqual(decoded, state, "round trip is lossless");
        assert.equal(decoded.intent, state.intent, "intent is never a storage address");
        assert.match(sessionStorageKey(identity), /^[a-f0-9]{64}$/);
        assert.equal(sessionStorageKey(identity), sessionStorageKey(identityOf(decoded)));
        assert.notEqual(sessionStorageKey(identity), sessionStorageKey({ ...identity, tenantScope: `${identity.tenantScope}:other` }));
        assert.throws(() => decodeSessionState(JSON.parse(encoded), { ...identity, tenantScope: `${identity.tenantScope}:other` }));
        assert.throws(() => decodeSessionState({ ...state, logicalSessionId: "00000000-0000-4000-8000-000000000000" }, identity));
        assert.throws(() => decodeSessionState({ ...state, schemaVersion: 0 }, identity));
        assert.throws(() => decodeSessionState({ ...state, seeds: "invalid" }, identity));
        for (const invalid of ["\0", "\ud800", "\udfff"]) {
          assert.throws(() => workflowIntentSchema.parse(`${state.intent}${invalid}`));
          assert.throws(() => encodeSessionState({ ...state, teachingTsv: invalid }, identity));
          assert.throws(() => decodeSessionState({ ...state, intent: `${state.intent}${invalid}${state.logicalSessionRef}` }, identity));
        }
        const next = workflowIntentSchema.parse("Read related details.");
        const chain = deriveIntent(decoded.intentProvenance, next);
        assert.deepEqual(chain.nodes.slice(0, -1), decoded.intentProvenance.nodes);
        assert.equal(chain.nodes.at(-1)?.parent, decoded.intentProvenance.nodes.length - 1);
        assert.deepEqual(intentProvenanceSchema.parse(JSON.parse(JSON.stringify(chain))), chain);
        assert.throws(() => intentProvenanceSchema.parse({ nodes: [{ parent: 0, intent: next }] }));
        if (index < 64) {
          const caseRoot = path.join(root, String(index));
          const store = new LocalSessionStore(caseRoot, state.tenantScope);
          await store.put(state);
          const reopened = new LocalSessionStore(caseRoot, state.tenantScope);
          assert.deepEqual(await reopened.get(state.logicalSessionRef), state);
          assert.deepEqual(await reopened.listSessions(), [state]);
          const manager = new SessionManager(reopened, state.tenantScope);
          assert.deepEqual(await manager.getByLogicalRef(state.logicalSessionRef), state);
          const stranger = new LocalSessionStore(caseRoot, `${state.tenantScope}:other`);
          assert.equal(await stranger.get(state.logicalSessionRef), null);
          await assert.rejects(() => stranger.put(state));
          const nextState = sessionCases(1, index + 1).next().value!;
          const sibling = { ...state, logicalSessionId: nextState.logicalSessionId, logicalSessionRef: nextState.logicalSessionRef };
          await reopened.put(sibling);
          assert.equal((await reopened.listSessions()).length, 2, "same intent retains separate sessions");
          assert.deepEqual(await reopened.get(state.logicalSessionRef), state);
          assert.deepEqual(await reopened.get(sibling.logicalSessionRef), sibling);
          // A correctly shaped record stored at another session's address must fail.
          const file = path.join(caseRoot, ".plasm", "sessions-v2", sessionTenantKey(state.tenantScope), `${sessionStorageKey(identity)}.json`);
          await writeFile(file, encodeSessionState(sibling, identityOf(sibling)));
          await assert.rejects(() => reopened.get(state.logicalSessionRef), /identity/);
          await assert.rejects(() => reopened.listSessions(), /identity/);
          await writeFile(file, encoded);
          assert.ok((await readdir(path.dirname(file))).every((name) => name.length < 80));
          assert.equal(JSON.parse(await readFile(file, "utf8")).intent, state.intent);
        }
      } catch (error) { throw new Error(`session property failed: seed=${seed}, case=${index}`, { cause: error }); }
      index++;
    }
    // Compile-time contract: neither prose nor an unvalidated string is an address.
    if (false) {
      const store = new LocalSessionStore(root);
      // @ts-expect-error prose is not a session ref
      await store.get(workflowIntentSchema.parse("read records"));
      // @ts-expect-error storage identity is not discovery prose
      deriveIntent(undefined, logicalSessionRefSchema.parse("l_invalid"));
    }
    console.log(`session-contract: ${count} generated codec/key/Unicode properties; 64 filesystem restart/isolation properties; seed=${seed}`);
  } finally { await rm(root, { recursive: true, force: true }); }
}

if (process.argv[1]?.endsWith("test-session-contract.ts")) await main();
