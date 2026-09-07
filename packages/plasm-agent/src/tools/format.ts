export function formatPlasmContextMarkdown(
  logicalSessionRef: string,
  tsv: string,
  reused: boolean,
): string {
  const delta = tsv.trim();
  const header = `**logical_session_ref:** \`${logicalSessionRef}\``;
  if (!delta) {
    if (reused) {
      return `${header}\n\nUnchanged — seeds already exposed. Next: \`plasm\` / \`plasm_run\`.\n`;
    }
    return `${header}\n`;
  }
  return `${header}\n\n\`\`\`tsv\n${delta}\n\`\`\`\n`;
}

export function formatPlasmDryRunMarkdown(summary: string, runRef: string): string {
  return `\`\`\`text\n${summary.trim()}\n\`\`\`\n\n**Run:** pass \`run_ref\`: \`${runRef}\` to **\`plasm_run\`**. Do not echo the program.`;
}

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
