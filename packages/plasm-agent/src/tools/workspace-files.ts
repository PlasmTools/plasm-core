import { constants } from "node:fs";
import { lstat, mkdir, open, realpath } from "node:fs/promises";
import path from "node:path";

/** Host writes reject symlink parents and never follow a symlink destination. */
export async function writeWorkspaceFile(root: string, relative: string, text: string): Promise<void> {
  if (path.isAbsolute(relative) || relative.split(/[\\/]/).some(p => p === ".." || p === "")) {
    throw new Error("Output path escapes workspace");
  }
  const base = await realpath(root);
  const segments = relative.split(/[\\/]/);
  const name = segments.pop()!;
  let parent = base;
  for (const segment of segments) {
    parent = path.join(parent, segment);
    await mkdir(parent).catch(error => { if (error.code !== "EEXIST") throw error; });
    const stat = await lstat(parent);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error("Output parent must be a real directory");
  }
  const file = await open(path.join(parent, name), constants.O_WRONLY | constants.O_CREAT | constants.O_TRUNC | constants.O_NOFOLLOW, 0o600);
  try { await file.writeFile(text, "utf8"); } finally { await file.close(); }
}
