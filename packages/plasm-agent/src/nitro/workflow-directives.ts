import { readdir, readFile } from "node:fs/promises";
import path from "node:path";

const WORKFLOW_MARKERS = ["use workflow", "use step", "'use workflow'", '"use workflow"'];

async function walkTsFiles(dir: string): Promise<string[]> {
  const out: string[] = [];
  let entries;
  try {
    entries = await readdir(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === ".plasm") continue;
      out.push(...(await walkTsFiles(full)));
      continue;
    }
    if (entry.name.endsWith(".ts") || entry.name.endsWith(".tsx")) {
      out.push(full);
    }
  }
  return out;
}

/** True when authored sources contain Workflow SDK directives (needs @workflow/nitro). */
export async function projectUsesWorkflowDirectives(projectRoot: string): Promise<boolean> {
  const roots = [
    path.join(projectRoot, "workflows"),
    path.join(projectRoot, "agent"),
    path.join(projectRoot, "lib"),
  ];
  for (const root of roots) {
    for (const file of await walkTsFiles(root)) {
      const source = await readFile(file, "utf8");
      if (WORKFLOW_MARKERS.some((marker) => source.includes(marker))) {
        return true;
      }
    }
  }
  return false;
}
