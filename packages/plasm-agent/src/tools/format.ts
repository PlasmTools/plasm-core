import type { ToolSet } from "ai";

/** Harness JS only after `plasm_read_run_artifact` materializes a snapshot. */
export function gateArtefactTransform(tools: ToolSet, artefactReady: boolean): ToolSet {
  if (artefactReady) return tools;
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

export const COMPLETE_TASK_TOOL_NAME = "complete_task";
export const SUBMIT_ANSWER_TOOL_NAME = "submit_answer";
