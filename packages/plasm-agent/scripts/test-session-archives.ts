import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { LocalArchiveStore } from "../src/archive/index.js";
const root = await mkdtemp(path.join(tmpdir(), "plasm-archives-"));
try {
  const archive = new LocalArchiveStore(root);
  const run = `pr${"a".repeat(64)}`;
  for (const session of ["session-a", "session-b"]) {
    await archive.writeRunSnapshot({run_id: run, plan_commit_ref: "pc0", logical_session_ref: session, ok:true, message:session, native_snapshot:{catalog:"owned-by-native",data:[1]}, archived_at:new Date().toISOString()});
  }
  assert.equal((await archive.getRun(run, "session-a"))?.message, "session-a");
  assert.equal((await archive.getRun(run, "session-b"))?.message, "session-b");
  assert.equal(await archive.getRun(run, "session-c"), null);
  console.log("PASS: identical canonical run IDs remain isolated by logical session");
} finally { await rm(root, {recursive:true,force:true}); }
