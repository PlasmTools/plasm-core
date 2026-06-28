export function formatPlasmContextMarkdown(
  logicalSessionRef: string,
  tsv: string,
  reused: boolean,
): string {
  const delta = tsv.trim();
  if (!delta) {
    if (reused) {
      return `\`${logicalSessionRef}\`\n\nUnchanged — seeds already exposed. Next: \`plasm\` / \`plasm_run\`.\n`;
    }
    return `\`${logicalSessionRef}\`\n`;
  }
  return `\`${logicalSessionRef}\`\n\n\`\`\`tsv\n${delta}\n\`\`\`\n`;
}

export function formatPlasmDryRunMarkdown(summary: string, runRef: string): string {
  return `\`\`\`text\n${summary.trim()}\n\`\`\`\n\n**Run:** pass \`run_ref\`: \`${runRef}\` to **\`plasm_run\`**. Do not echo the program.`;
}

export function formatPlasmRunMarkdown(
  message: string,
  ok: boolean,
  rowsJson?: string,
): string {
  if (!ok) return `**plasm_run** (pending transport)\n\n${message}`;
  const rows = rowsJson?.trim();
  if (!rows) return message;
  return `${message}\n\n\`\`\`json\n${rows}\n\`\`\``;
}
