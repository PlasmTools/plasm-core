import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { mkdir, realpath } from "node:fs/promises";
import { promisify } from "node:util";
import { z } from "zod";
import { writeWorkspaceFile } from "./workspace-files.js";

const execute = promisify(execFile);
let cleanupFailed = false;

/** Runs untrusted computation in a disposable container, never in the host realm. */
export async function runArtefactTransform(workspaceRoot: string, code: string): Promise<string> {
  if (cleanupFailed) throw new Error("Artifact runtime is disabled after a cleanup failure; restart after verifying container termination");
  const image = process.env.PLASM_ARTIFACT_IMAGE?.trim();
  if (!image || !/^[-a-zA-Z0-9./_:]+@sha256:[a-f0-9]{64}$/.test(image)) {
    throw new Error("Artifact execution requires PLASM_ARTIFACT_IMAGE pinned by sha256 digest");
  }
  if (Buffer.byteLength(code) > 128 * 1024) throw new Error("Artifact program exceeds 128 KiB");
  await mkdir(workspaceRoot, { recursive: true });
  const root = await realpath(workspaceRoot);
  if (root.includes(",")) throw new Error("Artifact workspace path cannot contain commas");
  const name = `plasm-artifact-${randomUUID()}`;
  const script = `
    const fs = require('node:fs/promises');
    const path = require('node:path');
    const outputs = new Set();
    const logs = [];
    const emit = process.stdout.write.bind(process.stdout);
    console.log = (...args) => logs.push(args.map(a => typeof a === 'string' ? a : JSON.stringify(a)).join(' '));
    console.error = console.log;
    async function resolve(rel, writing = false, root = '/work') {
      if (typeof rel !== 'string' || path.isAbsolute(rel)) throw Error('Relative path required');
      const full = path.resolve(root, rel);
      if (full !== root && !full.startsWith(root + '/')) throw Error('Path escapes workspace');
      let cursor = root;
      for (const part of path.relative(root, full).split('/').filter(Boolean)) {
        cursor = path.join(cursor, part);
        try { if ((await fs.lstat(cursor)).isSymbolicLink()) throw Error('Symlinks are not allowed'); }
        catch (e) { if (!(writing && e.code === 'ENOENT')) throw e; }
      }
      return full;
    }
    const readText = async rel => {
      try { return await fs.readFile(await resolve(rel), 'utf8'); }
      catch (e) { if (e.code !== 'ENOENT') throw e; return fs.readFile(await resolve(rel, false, '/input'), 'utf8'); }
    };
    const readJson = async rel => JSON.parse(await readText(rel));
    const writeText = async (rel, value) => {
      const full = await resolve(rel, true);
      await fs.mkdir(path.dirname(full), {recursive:true});
      await fs.writeFile(full, value, 'utf8'); outputs.add(rel);
    };
    const writeJson = async (rel, value) => writeText(rel, JSON.stringify(value, null, 2));
    const list = async (rel = '.') => [...new Set([...await fs.readdir(await resolve(rel, false, '/input')).catch(e=>{if(e.code==='ENOENT')return [];throw e;}), ...await fs.readdir(await resolve(rel)).catch(e=>{if(e.code==='ENOENT')return [];throw e;})])];
    const AsyncFunction = Object.getPrototypeOf(async function(){}).constructor;
    new AsyncFunction('readText','readJson','writeText','writeJson','list', ${JSON.stringify(code)})(
      readText, readJson, writeText, writeJson, list
    ).then(async value => {
      if (value !== undefined) console.log(value);
      const files = [];
      for (const rel of outputs) {
        const full = await resolve(rel);
        if (!(await fs.lstat(full)).isFile()) throw Error('Only regular output files may be exported');
        files.push({path:rel, text:await fs.readFile(full,'utf8')});
      }
      emit(JSON.stringify({text:logs.join('\\n'),files}));
    })
     .catch(error => { process.stderr.write(error.message); process.exitCode = 1; });
  `;
  let stdout: string;
  try {
    ({ stdout } = await execute("docker", [
      "run", "--rm", "--pull=never", "--name", name,
      "--network=none", "--read-only", "--cap-drop=ALL",
      "--security-opt=no-new-privileges", "--memory=128m", "--memory-swap=128m",
      "--cpus=1", "--pids-limit=64", "--ipc=none",
      "--tmpfs", "/tmp:rw,noexec,nosuid,size=8m",
      "--tmpfs", "/work:rw,noexec,nosuid,size=32m",
      "--mount", `type=bind,src=${root},dst=/input,readonly`, "--workdir=/work",
      "--entrypoint=timeout", image, "-s", "KILL", "30", "node", "-e", script,
    ], { timeout: 35_000, killSignal: "SIGKILL", maxBuffer: 1024 * 1024 }));
  } catch (error) {
    const failure = error as { stderr?: string; killed?: boolean; code?: unknown };
    throw new Error(failure.killed || failure.code === 137 ? "Artifact execution exceeded its resource limit"
      : `Artifact execution failed: ${(failure.stderr ?? String(failure.code ?? "runtime unavailable")).slice(0, 2000)}`);
  } finally {
    // Killing a Docker client does not kill its container. Always remove by unique name.
    await execute("docker", ["rm", "--force", name], { timeout: 10_000 }).catch(error => {
      if (!String(error.stderr).includes("No such container")) {
        cleanupFailed = true;
        throw new Error("Artifact container cleanup failed; runtime disabled until restart");
      }
    });
  }
  const output = z.object({text:z.string(), files:z.array(z.object({path:z.string(),text:z.string()}).strict()).max(64)}).strict().parse(JSON.parse(stdout));
  const exportRoot = `transforms/${randomUUID()}`;
  for (const file of output.files) await writeWorkspaceFile(root, `${exportRoot}/${file.path}`, file.text);
  return [output.text, ...output.files.map(file => `File: ${exportRoot}/${file.path}`)].filter(Boolean).join("\n") || "(no output)";
}
