const PROOF_HEADING = /^###\s+\[/;

/** Keep only `### [...]` proof blocks; drop agent preamble and troubleshooting narration. */
export function extractProofMarkdown(raw: string): string {
  const lines = raw.split(/\r?\n/);
  const blocks: string[] = [];
  let current: string[] = [];

  const flush = () => {
    if (current.length === 0) return;
    const block = current.join("\n").trim();
    if (block) blocks.push(block);
    current = [];
  };

  for (const line of lines) {
    if (PROOF_HEADING.test(line)) {
      flush();
      current.push(line);
      continue;
    }
    if (current.length > 0) {
      const trimmed = line.trim();
      if (trimmed === "") {
        current.push(line);
        continue;
      }
      if (line.startsWith("- ") || line.startsWith("* ")) {
        current.push(line);
        continue;
      }
      flush();
    }
  }
  flush();

  return blocks.join("\n\n").trim();
}

export function isMcpRelevant(title?: string, url?: string): boolean {
  const haystack = `${title ?? ""} ${url ?? ""}`;
  return /\b(MCP|Model Context Protocol|model[- ]context)\b/i.test(haystack);
}
