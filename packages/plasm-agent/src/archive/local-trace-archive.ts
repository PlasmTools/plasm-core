import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import path from "node:path";

import type { TraceDetail, TraceRecord, TraceSummary } from "./types.js";

function safeSegment(value: string): string {
  if (!value || value.includes("..") || value.includes("/") || value.includes("\\")) {
    throw new Error("invalid archive path segment");
  }
  return value;
}

export class LocalTraceArchive {
  constructor(private readonly root: string) {}

  private traceDir(tenantId: string, traceId: string): string {
    return path.join(this.root, "traces", safeSegment(tenantId), safeSegment(traceId));
  }

  async ensureRoot(): Promise<void> {
    await mkdir(this.root, { recursive: true });
  }

  async appendRecord(
    tenantId: string,
    traceId: string,
    record: TraceRecord,
  ): Promise<void> {
    const dir = this.traceDir(tenantId, traceId);
    await mkdir(dir, { recursive: true });
    const line = `${JSON.stringify(record)}\n`;
    await writeFile(path.join(dir, "records.ndjson"), line, { flag: "a" });
  }

  async persistTrace(detail: TraceDetail): Promise<void> {
    const dir = this.traceDir(detail.summary.tenant_id, detail.summary.trace_id);
    await mkdir(dir, { recursive: true });
    await writeFile(
      path.join(dir, "summary.json"),
      JSON.stringify(detail.summary, null, 2),
      "utf8",
    );
    const body = detail.records.map((r) => JSON.stringify(r)).join("\n");
    const suffix = body.length > 0 ? "\n" : "";
    await writeFile(path.join(dir, "records.ndjson"), `${body}${suffix}`, "utf8");
  }

  async listTraces(tenantId: string, limit = 50): Promise<TraceSummary[]> {
    const dir = path.join(this.root, "traces", safeSegment(tenantId));
    let entries: string[];
    try {
      entries = await readdir(dir);
    } catch {
      return [];
    }
    const summaries: TraceSummary[] = [];
    for (const traceId of entries) {
      try {
        const raw = await readFile(path.join(dir, traceId, "summary.json"), "utf8");
        summaries.push(JSON.parse(raw) as TraceSummary);
      } catch {
        continue;
      }
    }
    summaries.sort((a, b) => b.started_at_ms - a.started_at_ms);
    return summaries.slice(0, limit);
  }

  async getTrace(tenantId: string, traceId: string): Promise<TraceDetail | null> {
    const dir = this.traceDir(tenantId, traceId);
    let summaryRaw: string;
    try {
      summaryRaw = await readFile(path.join(dir, "summary.json"), "utf8");
    } catch {
      return null;
    }
    const summary = JSON.parse(summaryRaw) as TraceSummary;
    let records: TraceRecord[] = [];
    try {
      const ndjson = await readFile(path.join(dir, "records.ndjson"), "utf8");
      records = ndjson
        .split("\n")
        .map((line) => line.trim())
        .filter(Boolean)
        .map((line) => JSON.parse(line) as TraceRecord);
    } catch {
      records = [];
    }
    return { summary, records };
  }
}
