import { cp, lstat, readlink, readdir, rm } from "node:fs/promises";
import path from "node:path";

import { vercelOutputDir } from "./paths.js";

async function pathExists(p: string): Promise<boolean> {
  try {
    await lstat(p);
    return true;
  } catch {
    return false;
  }
}

async function collectSymlinks(rootDir: string): Promise<string[]> {
  const symlinks: string[] = [];
  async function walk(dir: string): Promise<void> {
    let entries: Array<{ name: string; isDirectory(): boolean; isSymbolicLink(): boolean }>;
    try {
      entries = await readdir(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isSymbolicLink()) {
        symlinks.push(fullPath);
        continue;
      }
      if (entry.isDirectory()) {
        await walk(fullPath);
      }
    }
  }
  await walk(rootDir);
  return symlinks;
}

function isExternalSymlink(funcDir: string, linkPath: string, target: string): boolean {
  const resolved = path.resolve(path.dirname(linkPath), target);
  return resolved !== funcDir && !resolved.startsWith(`${funcDir}${path.sep}`);
}

async function materializeExternalSymlinks(funcDir: string): Promise<number> {
  const symlinks = await collectSymlinks(funcDir);
  let materialized = 0;
  for (const linkPath of symlinks) {
    let target: string;
    try {
      target = await readlink(linkPath);
    } catch {
      continue;
    }
    if (!isExternalSymlink(funcDir, linkPath, target)) {
      continue;
    }
    const resolved = path.resolve(path.dirname(linkPath), target);
    if (!(await pathExists(resolved))) {
      continue;
    }
    await rm(linkPath, { force: true });
    await cp(resolved, linkPath, { recursive: true, dereference: true });
    materialized += 1;
  }
  return materialized;
}

async function findWorkflowFuncDirs(outputRoot: string): Promise<string[]> {
  const workflowRoot = path.join(outputRoot, "functions", ".well-known", "workflow");
  if (!(await pathExists(workflowRoot))) {
    return [];
  }
  const funcDirs: string[] = [];
  async function walk(dir: string): Promise<void> {
    let entries: Array<{ name: string; isDirectory(): boolean }>;
    try {
      entries = await readdir(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const fullPath = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name.endsWith(".func")) {
          funcDirs.push(fullPath);
        } else {
          await walk(fullPath);
        }
      }
    }
  }
  await walk(workflowRoot);
  return funcDirs;
}

export async function materializeWorkflowFunctionDeps(projectRoot: string): Promise<void> {
  const outputRoot = vercelOutputDir(projectRoot);
  const funcDirs = await findWorkflowFuncDirs(outputRoot);
  for (const funcDir of funcDirs) {
    for (let pass = 0; pass < 4; pass += 1) {
      const changed = await materializeExternalSymlinks(funcDir);
      if (changed === 0) break;
    }
  }
}
