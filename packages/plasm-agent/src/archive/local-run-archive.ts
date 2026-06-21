import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";

import type { PlanArchiveSnapshot, RunSnapshot } from "./types.js";

function safeFilename(value: string): string {
  if (!value || value.includes("..") || value.includes("/") || value.includes("\\")) {
    throw new Error("invalid archive filename");
  }
  return value;
}

export class LocalRunArchive {
  constructor(private readonly root: string) {}

  private plansDir(): string {
    return path.join(this.root, "plans");
  }

  private runsDir(): string {
    return path.join(this.root, "runs");
  }

  async ensureRoot(): Promise<void> {
    await mkdir(this.plansDir(), { recursive: true });
    await mkdir(this.runsDir(), { recursive: true });
  }

  async writePlanArchive(snapshot: PlanArchiveSnapshot): Promise<void> {
    await this.ensureRoot();
    const file = path.join(this.plansDir(), `${safeFilename(snapshot.plan_commit_ref)}.json`);
    await writeFile(file, JSON.stringify(snapshot, null, 2), "utf8");
  }

  async writeRunSnapshot(snapshot: RunSnapshot): Promise<void> {
    await this.ensureRoot();
    const file = path.join(this.runsDir(), `${safeFilename(snapshot.run_id)}.json`);
    await writeFile(file, JSON.stringify(snapshot, null, 2), "utf8");
  }

  async listPlans(limit = 50): Promise<PlanArchiveSnapshot[]> {
    return this.listJsonDir<PlanArchiveSnapshot>(this.plansDir(), limit);
  }

  async listRuns(limit = 50): Promise<RunSnapshot[]> {
    return this.listJsonDir<RunSnapshot>(this.runsDir(), limit);
  }

  async getPlan(planCommitRef: string): Promise<PlanArchiveSnapshot | null> {
    return this.readJson<PlanArchiveSnapshot>(
      path.join(this.plansDir(), `${safeFilename(planCommitRef)}.json`),
    );
  }

  async getRun(runId: string): Promise<RunSnapshot | null> {
    return this.readJson<RunSnapshot>(
      path.join(this.runsDir(), `${safeFilename(runId)}.json`),
    );
  }

  private async listJsonDir<T>(dir: string, limit: number): Promise<T[]> {
    let files: string[];
    try {
      files = await readdir(dir);
    } catch {
      return [];
    }
    const items: Array<{ at: string; value: T }> = [];
    for (const file of files.filter((f) => f.endsWith(".json"))) {
      const value = await this.readJson<T>(path.join(dir, file));
      if (!value) continue;
      const at =
        (value as { archived_at?: string }).archived_at ??
        (value as { plan_commit_ref?: string }).plan_commit_ref ??
        file;
      items.push({ at, value });
    }
    items.sort((a, b) => b.at.localeCompare(a.at));
    return items.slice(0, limit).map((item) => item.value);
  }

  private async readJson<T>(filePath: string): Promise<T | null> {
    try {
      const raw = await readFile(filePath, "utf8");
      return JSON.parse(raw) as T;
    } catch {
      return null;
    }
  }
}
