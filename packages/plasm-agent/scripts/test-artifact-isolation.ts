import assert from "node:assert/strict";
import { mkdtemp, mkdir, writeFile, readFile, symlink, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { runArtefactTransform } from "../src/tools/artifact-process.js";
import { writeWorkspaceFile } from "../src/tools/workspace-files.js";

const root = await mkdtemp(path.join(tmpdir(), "plasm-isolation-"));
try {
  const workspace = path.join(root, "work");
  await mkdir(workspace);
  await writeFile(path.join(root, "outside"), "DISPOSABLE_OUTSIDE_MARKER");
  await symlink(path.join(root, "outside"), path.join(workspace, "link"));
  process.env.PLASM_AUDIT_HOST_SENTINEL = "HOST_ONLY";
  const computed = await runArtefactTransform(workspace, 'await writeJson("value.json", {n: 7}); return (await readJson("value.json")).n;');
  assert.match(computed, /^7\nFile: transforms\//);
  const exported = computed.split("File: ")[1]!;
  assert.equal(JSON.parse(await readFile(path.join(workspace, exported), "utf8")).n, 7);
  assert.equal(await runArtefactTransform(workspace, 'return typeof process.env.PLASM_AUDIT_HOST_SENTINEL;'), "undefined");
  await assert.rejects(runArtefactTransform(workspace, 'return readText("link");'), /Symlinks/);
  await assert.rejects(runArtefactTransform(workspace, 'return readText("../outside");'), /escapes/);
  await assert.rejects(runArtefactTransform(workspace, 'await fetch("http://192.0.2.1", {signal: AbortSignal.timeout(1000)});'), /failed/);
  await assert.rejects(runArtefactTransform(workspace, 'await require("node:fs/promises").writeFile("/outside", "x");'), /read-only/);
  await assert.rejects(runArtefactTransform(workspace, 'await require("node:fs/promises").symlink("/outside", "/input/tool-trace.jsonl");'), /read-only/);
  await assert.rejects(runArtefactTransform(workspace, 'await writeText("attack", "x"); const fs = require("node:fs/promises"); await fs.unlink("/work/attack"); await fs.symlink("/outside", "/work/attack");'), /Symlinks/);
  await assert.rejects(writeWorkspaceFile(workspace, "link", "REPLACED"));
  assert.equal(await readFile(path.join(root, "outside"), "utf8"), "DISPOSABLE_OUTSIDE_MARKER");
  await assert.rejects(runArtefactTransform(workspace, 'const fs = require("node:fs/promises"); await fs.writeFile("/work/large", Buffer.alloc(40 * 1024 * 1024));'), /space|resource limit/);
  await assert.rejects(runArtefactTransform(workspace, 'await new Promise(r => setTimeout(r, 60000));'), /resource limit/);
  console.log("PASS: artifact compute, host env isolation, symlink/traversal rejection, no network, readonly root, async deadline");
} finally {
  delete process.env.PLASM_AUDIT_HOST_SENTINEL;
  await rm(root, {recursive: true, force: true});
}
