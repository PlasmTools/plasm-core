import type { ModelMessage, ToolSet } from "ai";

import { COMPLETE_TASK_TOOL_NAME, SUBMIT_ANSWER_TOOL_NAME } from "./format.js";

const RUN_ID_RE = /\bpr[a-f0-9]{64}\b/g;
const ARTIFACT_URI_RE = /plasm:\/\/[^\s`"'<>]+/g;

/** Canonical `pr` + 64 hex run snapshot id, or null. */
export function runIdFromArtifactRef(raw: string): string | null {
  const trimmed = raw.trim();
  if (/^pr[a-f0-9]{64}$/.test(trimmed)) return trimmed;
  const fromPath = /\/(?:run|artifacts)\/(pr[a-f0-9]{64})\b/.exec(trimmed);
  if (fromPath?.[1]) return fromPath[1];
  const any = /\b(pr[a-f0-9]{64})\b/.exec(trimmed);
  return any?.[1] ?? null;
}

/**
 * Preview is not the full rowset — house law: read the snapshot before concluding
 * long text / reference-only fields are missing.
 */
export function previewRequiresArtifact(text: string): boolean {
  if (!text) return false;
  if (/\(in artifact\)/i.test(text)) return true;
  if (/result_delivery\s*[:\t]\s*snapshot_only/i.test(text)) return true;
  if (/needs_artifact:\s*true/i.test(text)) return true;
  if (/\bresource_link\b/i.test(text) && /plasm:\/\//i.test(text)) return true;
  return false;
}

/** `pr…` ids and `plasm://` URIs advertised in a tool result. */
export function artifactKeysInText(text: string): string[] {
  const keys = new Set<string>();
  ARTIFACT_URI_RE.lastIndex = 0;
  for (const match of text.matchAll(ARTIFACT_URI_RE)) {
    keys.add(match[0].replace(/[.,;]+$/, ""));
  }
  RUN_ID_RE.lastIndex = 0;
  for (const match of text.matchAll(RUN_ID_RE)) {
    keys.add(match[0]);
  }
  return [...keys];
}

export function artifactRefFromToolInput(input: unknown): string | null {
  if (typeof input === "string") {
    try {
      return artifactRefFromToolInput(JSON.parse(input));
    } catch {
      const uri = /"artifact_uri"\s*:\s*"([^"]+)"/.exec(input);
      if (uri?.[1]) return uri[1];
      const run = /"run_id"\s*:\s*"([^"]+)"/.exec(input);
      return run?.[1] ?? null;
    }
  }
  if (!input || typeof input !== "object") return null;
  const rec = input as { artifact_uri?: unknown; run_id?: unknown };
  if (typeof rec.artifact_uri === "string" && rec.artifact_uri.trim()) {
    return rec.artifact_uri.trim();
  }
  if (typeof rec.run_id === "string" && rec.run_id.trim()) {
    return rec.run_id.trim();
  }
  return null;
}

function partText(part: unknown): string {
  if (typeof part === "string") return part;
  if (!part || typeof part !== "object") return "";
  const rec = part as Record<string, unknown>;
  if (typeof rec.text === "string") return rec.text;
  if (typeof rec.result === "string") return rec.result;
  if (typeof rec.output === "string") return rec.output;
  const output = rec.output;
  if (output && typeof output === "object" && typeof (output as { value?: unknown }).value === "string") {
    return (output as { value: string }).value;
  }
  return "";
}

function toolCallIdOf(rec: Record<string, unknown>): string {
  if (typeof rec.toolCallId === "string") return rec.toolCallId;
  if (typeof rec.toolCallID === "string") return rec.toolCallID;
  return "";
}

function toolPartIsError(rec: Record<string, unknown>): boolean {
  if (rec.type === "tool-error" || rec.type === "error") return true;
  if (rec.isError === true) return true;
  const output = rec.output;
  if (output && typeof output === "object") {
    const t = (output as { type?: unknown }).type;
    if (t === "error-text" || t === "error-json" || t === "error") return true;
  }
  return false;
}

function liveRunFailed(text: string): boolean {
  return /\*\*plasm(?:_run)?\*\* error/i.test(text) || /pending transport/i.test(text);
}

export function clearArtifactKey(outstanding: Set<string>, key: string): void {
  outstanding.delete(key);
  const runId = runIdFromArtifactRef(key);
  if (runId) outstanding.delete(runId);
  if (!runId) return;
  for (const existing of [...outstanding]) {
    if (existing.includes(runId)) outstanding.delete(existing);
  }
}

function consumedArtifactReads(messages: ModelMessage[]): Map<string, string> {
  const consumed = new Map<string, string>();
  for (const message of messages) {
    if (message.role !== "assistant") continue;
    const content = message.content;
    if (!Array.isArray(content)) continue;
    for (const part of content) {
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      if (rec.toolName !== "plasm_read_run_artifact") continue;
      const ref = artifactRefFromToolInput(rec.input ?? rec.args);
      if (!ref) continue;
      consumed.set(toolCallIdOf(rec) || `__anon_${consumed.size}`, ref);
    }
  }
  return consumed;
}

/**
 * Unread required snapshots still waiting for `plasm_read_run_artifact`.
 * Inline TSV without `(in artifact)` / `snapshot_only` / `resource_link` is complete.
 */
export function applyArtifactLedger(
  outstanding: Set<string>,
  messages: ModelMessage[],
): void {
  const consumed = consumedArtifactReads(messages);
  for (const message of messages) {
    if (message.role !== "tool") continue;
    const content = message.content;
    if (typeof content === "string") {
      if (previewRequiresArtifact(content) && !liveRunFailed(content)) {
        for (const key of artifactKeysInText(content)) outstanding.add(key);
      }
      continue;
    }
    if (!Array.isArray(content)) continue;
    for (const part of content) {
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      const toolName = typeof rec.toolName === "string" ? rec.toolName : "";
      const text = partText(part);
      if (toolName === "plasm_read_run_artifact") {
        if (toolPartIsError(rec) || /plasm_read_run_artifact:/i.test(text)) continue;
        const id = toolCallIdOf(rec);
        const handled =
          (id && consumed.get(id)) ||
          (consumed.size === 1 ? [...consumed.values()][0] : undefined) ||
          artifactKeysInText(text)[0];
        if (handled) clearArtifactKey(outstanding, handled);
        continue;
      }
      if (toolName !== "plasm" && toolName !== "plasm_run" && toolName !== "") continue;
      if (liveRunFailed(text) || !previewRequiresArtifact(text)) continue;
      for (const key of artifactKeysInText(text)) outstanding.add(key);
    }
  }
}

function executableArtifactReadCalls(refs: string[]): string[] {
  const seenRunIds = new Set<string>();
  const calls: string[] = [];
  for (const ref of refs) {
    if (!ref.startsWith("plasm://")) continue;
    const runId = runIdFromArtifactRef(ref);
    if (runId) seenRunIds.add(runId);
    calls.push(`plasm_read_run_artifact with ${JSON.stringify({ artifact_uri: ref })}`);
  }
  for (const ref of refs) {
    if (ref.startsWith("plasm://")) continue;
    const runId = runIdFromArtifactRef(ref) ?? ref;
    if (seenRunIds.has(runId)) continue;
    seenRunIds.add(runId);
    calls.push(`plasm_read_run_artifact with ${JSON.stringify({ run_id: ref })}`);
  }
  return calls;
}

/** Exact remaining snapshot locators plus an executable `plasm_read_run_artifact` call. */
export function unreadArtifactTerminalError(outstanding: ReadonlySet<string>): string {
  const remaining = [...outstanding].sort();
  const calls = executableArtifactReadCalls(remaining);
  return `unread run snapshot — remaining: ${remaining.join(", ")} — call ${calls.join("; call ")}`;
}

/** Terminal tools must not succeed while a required snapshot is still unread. */
export function gateUnreadArtifactTerminals(
  tools: ToolSet,
  outstandingArtifacts: ReadonlySet<string>,
): ToolSet {
  if (outstandingArtifacts.size === 0) return tools;
  const message = unreadArtifactTerminalError(outstandingArtifacts);
  const next: ToolSet = { ...tools };
  for (const name of [COMPLETE_TASK_TOOL_NAME, SUBMIT_ANSWER_TOOL_NAME]) {
    const existing = next[name];
    if (!existing || typeof existing !== "object") continue;
    next[name] = {
      ...existing,
      execute: async () => {
        throw new Error(message);
      },
    } as ToolSet[string];
  }
  return next;
}
