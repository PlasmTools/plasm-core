#!/usr/bin/env node
import assert from "node:assert/strict";
import { checkSessionExtension } from "./test-session-extension.js";

let entered!: () => void;
let release!: () => void;
const inDiscovery = new Promise<void>(resolve => { entered = resolve; });
const proceed = new Promise<void>(resolve => { release = resolve; });

await checkSessionExtension(
  async intent => {
    if (intent === "Delayed terminal race") {
      entered();
      await proceed;
    }
  },
  async (runtime, ref) => {
    const pending = runtime.plasmContext({ intent: "Delayed terminal race", sessionMode: "new" });
    const outcome = pending.then(() => "resolved", error => error);
    await inDiscovery;
    await runtime.sealEffectWorkflows();
    release();
    const result = await outcome;
    assert.ok(result instanceof Error, "a context started before sealing cannot reopen the terminal");
    assert.match(result.message, /Workflow completion changed/);
    await assert.rejects(runtime.plasmRun({ logicalSessionRef: ref, runRef: "pc1" }), /already ended/);
    const next = await runtime.plasmContext({ intent: "Explicit new workflow", sessionMode: "new" });
    assert.match(next, /l_[A-Za-z0-9_-]{22}/);
  },
);
