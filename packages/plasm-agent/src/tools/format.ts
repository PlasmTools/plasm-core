import type { ToolSet } from "ai";

import { pinnedArtifactImage } from "./artifact-process.js";

/** Advertise transform only when a snapshot exists and the digest pin is valid. */
export function artefactTransformAdvertised(artefactReady: boolean): boolean {
  return artefactReady && pinnedArtifactImage() !== null;
}

/** Harness JS only after `plasm_read_run_artifact` and a pinned image. */
export function gateArtefactTransform(tools: ToolSet, artefactReady: boolean): ToolSet {
  if (artefactTransformAdvertised(artefactReady)) return tools;
  const { plasm_artefact_transform: _omit, ...rest } = tools;
  return rest;
}

export function formatPlasmContextMarkdown(
  logicalSessionRef: string,
  tsv: string,
  reused: boolean,
  opts?: { teachingTsv?: string },
): string {
  const delta = tsv.trim();
  const header = `**logical_session_ref:** \`${logicalSessionRef}\``;
  // Live teaching delta (new open or real extend) — full language card, never a compressed roster.
  if (delta) {
    const reuseNote = reused
      ? `\n\nUnchanged — seeds already exposed. **Do not call plasm_context again** until you need new entities.\n`
      : `\n\n`;
    return `${header}${reuseNote}\`\`\`tsv\n${delta}\n\`\`\`\n`;
  }
  // Seedless reuse: protocol only. Re-dumping the full card rewards plasm_context spam
  // after successful reads (card already in transcript from the open wave).
  if (reused) {
    return [
      header,
      "",
      "Unchanged — seeds already exposed. **Do not call plasm_context again** until you need new entities.",
      "Reuse this ref on **plasm** / **plasm_run**.",
    ].join("\n");
  }
  const card = (opts?.teachingTsv ?? "").trim();
  if (!card) return `${header}\n`;
  return `${header}\n\n\`\`\`tsv\n${card}\n\`\`\`\n`;
}

/** Protocol-only dry surface — domain meaning lives in the language card / CGS. */
export function formatPlasmDryRunMarkdown(summary: string, runRef: string): string {
  return (
    `\`\`\`text\n${summary.trim()}\n\`\`\`\n\n` +
    `**Run:** pass \`run_ref\`: \`${runRef}\` to **\`plasm_run\`**. Do not echo the program.`
  );
}

/** Protocol-only run surface — no task coaching. */
export function formatPlasmRunMarkdown(
  message: string,
  ok: boolean,
  rowsJson?: string,
  runId?: string,
): string {
  if (!ok) return `**plasm_run** (pending transport)\n\n${message}`;
  const runLine = runId
    ? `\n\n**run_id:** \`${runId}\` — call **plasm_read_run_artifact** with this id when you need the full snapshot.`
    : "";
  const rows = rowsJson?.trim();
  if (!rows) return `${message}${runLine}`;
  return `${message}${runLine}\n\n\`\`\`json\n${rows}\n\`\`\``;
}

/** Dry-run schedule line (`1n 0r 1w`) — write count only. */
export function writeCountFromSummary(summary: string): number {
  const m = summary.match(/\b(\d+)w\b/);
  return m ? Number(m[1]) : 0;
}

const NULLISH_CELL = new Set(["", "null", "(null)", "none", "nil", "n/a", "na"]);

function cellIsNullish(val: string): boolean {
  return NULLISH_CELL.has(val.trim().toLowerCase());
}

function coerceNumber(val: string): number | null {
  const t = val.trim();
  if (!t || /[^0-9.+-eE]/.test(t)) return null;
  const n = Number(t);
  return Number.isFinite(n) ? n : null;
}

/** Categorical / identity ask — must not be answered with a count/sum. */
export function taskPrefersLabelAnswer(task: string): boolean {
  if (/\b(how\s+many|how\s+much|total|sum|aggregate|count\s+of)\b/i.test(task)) {
    return false;
  }
  return /\b(which|who|whom|whose|name|label|what)\b/i.test(task);
}

function labelFromRow(cells: string[]): string | null {
  for (const raw of cells) {
    const val = raw.trim();
    if (cellIsNullish(val) || coerceNumber(val) !== null) continue;
    return val;
  }
  return null;
}

function numberFromRow(headers: string[], cells: string[]): string | null {
  const prefer = ["sum", "total", "amount", "count", "n"];
  for (let i = 0; i < headers.length; i++) {
    const hl = headers[i]?.toLowerCase() ?? "";
    if (i < cells.length && prefer.some((p) => hl === p || hl.includes(p))) {
      const n = coerceNumber(cells[i] ?? "");
      if (n !== null) return String(n);
    }
  }
  if (cells.length === 1) {
    const n = coerceNumber(cells[0] ?? "");
    if (n !== null) return String(n);
    const t = cells[0]?.trim() ?? "";
    return cellIsNullish(t) ? null : t;
  }
  for (const c of cells) {
    const n = coerceNumber(c);
    if (n !== null) return String(n);
  }
  return null;
}

/**
 * Host scalar from the last return TSV. Singleton rows and ranking tables
 * (first data row) share one law — no hand-sum of dumps.
 */
export function extractGradedScalarFromObservation(
  resultText: string,
  task = "",
): string | null {
  const fences = [...resultText.matchAll(/```tsv\n([\s\S]*?)```/g)];
  if (fences.length === 0) return null;
  const body = fences[fences.length - 1]?.[1] ?? "";
  const lines = body.split("\n").map((ln) => ln.trim()).filter(Boolean);
  const [header, row] = lines;
  if (header === undefined || row === undefined) return null;
  const headers = header.split("\t").map((h) => h.trim());
  const cells = row.split("\t").map((c) => c.trim());
  if (taskPrefersLabelAnswer(task)) {
    return labelFromRow(cells);
  }
  return numberFromRow(headers, cells);
}

/** Eval / AppWorld side-effect terminator — not a Plasm language primitive. */
export const COMPLETE_TASK_TOOL_NAME = "complete_task";

/** Eval / AppWorld reportable-value terminator — not a Plasm language primitive. */
export const SUBMIT_ANSWER_TOOL_NAME = "submit_answer";

export function isEvalTerminalTool(name: string): boolean {
  return name === COMPLETE_TASK_TOOL_NAME || name === SUBMIT_ANSWER_TOOL_NAME;
}

function toolInputOf(part: Record<string, unknown>): unknown {
  return part.input ?? part.args;
}

function parseToolInput(raw: unknown): Record<string, unknown> | null {
  if (typeof raw === "string") {
    try {
      return parseToolInput(JSON.parse(raw));
    } catch {
      return null;
    }
  }
  if (raw && typeof raw === "object" && !Array.isArray(raw)) {
    return raw as Record<string, unknown>;
  }
  return null;
}

/** Non-empty string `answer` on `submit_answer`. Empty / missing / non-string is invalid. */
export function validSubmitAnswerPayload(input: unknown): string | null {
  const parsed = parseToolInput(input);
  if (!parsed || !Object.prototype.hasOwnProperty.call(parsed, "answer")) return null;
  const answer = parsed.answer;
  if (typeof answer !== "string" || answer.trim().length === 0) return null;
  return answer;
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

function toolCallIdOfPart(rec: Record<string, unknown>): string {
  if (typeof rec.toolCallId === "string") return rec.toolCallId;
  if (typeof rec.toolCallID === "string") return rec.toolCallID;
  return "";
}

export type SuccessfulEvalTerminal =
  | { tool: typeof COMPLETE_TASK_TOOL_NAME; answer: null }
  | { tool: typeof SUBMIT_ANSWER_TOOL_NAME; answer: string };

function recordSuccessfulResults(
  messages: ReadonlyArray<{ role?: string; content?: unknown }>,
): { ok: Set<string>; err: Set<string> } {
  const ok = new Set<string>();
  const err = new Set<string>();
  for (const message of messages) {
    if (message.role !== "tool") continue;
    const content = message.content;
    if (!Array.isArray(content)) continue;
    for (const part of content) {
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      const name = typeof rec.toolName === "string" ? rec.toolName : "";
      if (!isEvalTerminalTool(name)) continue;
      const id = toolCallIdOfPart(rec) || name;
      if (toolPartIsError(rec)) err.add(id);
      else ok.add(id);
    }
  }
  return { ok, err };
}

/**
 * Last **successful** eval terminal: execute ran without error and the payload
 * is valid. `complete_task` → explicit null. `submit_answer` → verbatim answer.
 * Call-only / schema-miss / thrown execute / empty submit are not successful.
 */
export function lastSuccessfulEvalTerminal(
  messages: ReadonlyArray<{ role?: string; content?: unknown }>,
): SuccessfulEvalTerminal | null {
  const { ok, err } = recordSuccessfulResults(messages);
  let found: SuccessfulEvalTerminal | null = null;
  for (const message of messages) {
    if (message.role !== "assistant") continue;
    const content = message.content;
    if (!Array.isArray(content)) continue;
    for (const part of content) {
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      const name = typeof rec.toolName === "string" ? rec.toolName : "";
      if (!isEvalTerminalTool(name)) continue;
      const id = toolCallIdOfPart(rec) || name;
      if (err.has(id) || (!ok.has(id) && !ok.has(name))) continue;
      if (name === COMPLETE_TASK_TOOL_NAME) {
        found = { tool: COMPLETE_TASK_TOOL_NAME, answer: null };
        continue;
      }
      const answer = validSubmitAnswerPayload(toolInputOf(rec));
      if (answer == null) continue;
      found = { tool: SUBMIT_ANSWER_TOOL_NAME, answer };
    }
  }
  return found;
}

export function successfulEvalTerminalInStep(opts: {
  toolCalls: ReadonlyArray<{
    toolName: string;
    input?: unknown;
    args?: unknown;
    toolCallId?: string;
  }>;
  toolResults?: ReadonlyArray<Record<string, unknown>>;
  messages?: ReadonlyArray<{ role?: string; content?: unknown }>;
}): SuccessfulEvalTerminal | null {
  const fromMessages = lastSuccessfulEvalTerminal(opts.messages ?? []);
  if (fromMessages) return fromMessages;

  const results = opts.toolResults ?? [];
  const okIds = new Set<string>();
  const errIds = new Set<string>();
  for (const rec of results) {
    const name = typeof rec.toolName === "string" ? rec.toolName : "";
    if (!isEvalTerminalTool(name)) continue;
    const id = toolCallIdOfPart(rec) || name;
    if (toolPartIsError(rec)) errIds.add(id);
    else okIds.add(id);
  }
  if (okIds.size === 0) return null;

  let found: SuccessfulEvalTerminal | null = null;
  for (const call of opts.toolCalls) {
    if (!isEvalTerminalTool(call.toolName)) continue;
    const id = call.toolCallId || call.toolName;
    if (errIds.has(id) || (!okIds.has(id) && !okIds.has(call.toolName))) continue;
    if (call.toolName === COMPLETE_TASK_TOOL_NAME) {
      found = { tool: COMPLETE_TASK_TOOL_NAME, answer: null };
      continue;
    }
    const answer = validSubmitAnswerPayload(call.input ?? call.args);
    if (answer == null) continue;
    found = { tool: SUBMIT_ANSWER_TOOL_NAME, answer };
  }
  return found;
}

/**
 * Last **valid** `submit_answer` payload only.
 * Invalid / empty / `{}` is ignored — it does not become null.
 * `complete_task` never grades. Chat / streamText / table extract is never an answer.
 */
export function submittedAnswerFromSubmitAnswer(
  messages: ReadonlyArray<{ role?: string; content?: unknown }>,
): string | null {
  let found: string | null = null;
  for (const message of messages) {
    if (message.role !== "assistant") continue;
    const content = message.content;
    if (!Array.isArray(content)) continue;
    for (const part of content) {
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      if (rec.toolName !== SUBMIT_ANSWER_TOOL_NAME) continue;
      const answer = validSubmitAnswerPayload(toolInputOf(rec));
      if (answer == null) continue;
      found = answer;
    }
  }
  return found;
}

/**
 * Terminal graded answer is the agent's submitted `submit_answer` text.
 * Host TSV extract, task-wording kind, and chat narration must not replace it.
 * Empty / omitted text → null only as a pass-through of an already-chosen value.
 */
export function gradedScalarAfterLiveWrites(opts: { text?: string | null }): string | null {
  const trimmed = (opts.text ?? "").trim();
  return trimmed.length > 0 ? trimmed : null;
}

export type EvalTerminalGrade =
  | { kind: "unterminated" }
  | { kind: "null"; tool: typeof COMPLETE_TASK_TOOL_NAME }
  | { kind: "scalar"; tool: typeof SUBMIT_ANSWER_TOOL_NAME; answer: string };

/**
 * Validated termination only. `budget_exhausted`, `unterminated` (mid-budget
 * model stop), leftover chat, and a missing or failed terminal are grade
 * `unterminated` — not explicit null.
 */
export function evalTerminalGrade(opts: {
  messages: ReadonlyArray<{ role?: string; content?: unknown }>;
  stopReason?: string;
}): EvalTerminalGrade {
  if (opts.stopReason != null && opts.stopReason !== "completed") {
    return { kind: "unterminated" };
  }
  const terminal = lastSuccessfulEvalTerminal(opts.messages);
  if (terminal == null) return { kind: "unterminated" };
  if (terminal.tool === COMPLETE_TASK_TOOL_NAME) {
    return { kind: "null", tool: COMPLETE_TASK_TOOL_NAME };
  }
  return { kind: "scalar", tool: SUBMIT_ANSWER_TOOL_NAME, answer: terminal.answer };
}

/**
 * CUGA eval teardown injects `apis.supervisor.complete_task(...)`.
 * Unterminated must not become `status='success', answer=''` (T183304 hole:
 * empty/None both `answer_to_text` to `null`, so answers-match fake-passes).
 */
export function hostMayInjectAppWorldComplete(grade: EvalTerminalGrade): boolean {
  return grade.kind === "null" || grade.kind === "scalar";
}
