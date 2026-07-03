#!/usr/bin/env node
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import { resolvePlasmAppOptions } from "../src/server/resolve-app-options.js";

const dir = mkdtempSync(path.join(tmpdir(), "plasm-resolve-app-"));
const agentRoot = path.join(dir, "agent");
const projectRoot = dir;

writeFileSync(path.join(projectRoot, ".env"), "PLASM_TENANT_SCOPE=test-radar\n");

delete process.env.PLASM_TENANT_SCOPE;

const resolved = resolvePlasmAppOptions(agentRoot, {
  agentRoot,
  definition: { model: "anthropic/claude-sonnet-4.6" },
});
assert.equal(resolved.tenantScope, "test-radar");
assert.equal(resolved.maxSteps, 24);
assert.equal(resolved.telemetry, true);
assert.equal(typeof resolved.hostTransport, "function");

console.log("resolve-app-options: ok");
