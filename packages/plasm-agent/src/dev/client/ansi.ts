export const ANSI = {
  reset: "\x1b[0m",
  bold: "\x1b[1m",
  dim: "\x1b[2m",
  cyan: "\x1b[36m",
  green: "\x1b[32m",
  yellow: "\x1b[33m",
  red: "\x1b[31m",
  magenta: "\x1b[35m",
  blue: "\x1b[34m",
} as const;

export function supportsColor(): boolean {
  if (process.env.NO_COLOR !== undefined) return false;
  return Boolean(process.stdout.isTTY);
}

export function paint(text: string, ...styles: string[]): string {
  if (!supportsColor()) return text;
  return `${styles.join("")}${text}${ANSI.reset}`;
}

export function wrap(text: string, width = 78): string[] {
  const lines: string[] = [];
  for (const raw of text.split(/\r?\n/)) {
    let line = raw;
    while (line.length > width) {
      let breakAt = line.lastIndexOf(" ", width);
      if (breakAt <= 0) breakAt = width;
      lines.push(line.slice(0, breakAt).trimEnd());
      line = line.slice(breakAt).trimStart();
    }
    lines.push(line);
  }
  return lines;
}
