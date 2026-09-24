/** Canonical `pr` + 64 hex run snapshot id, or null. */
export function runIdFromArtifactRef(raw: string): string | null {
  const trimmed = raw.trim();
  if (/^pr[a-f0-9]{64}$/.test(trimmed)) return trimmed;
  const fromPath = /\/(?:run|artifacts)\/(pr[a-f0-9]{64})\b/.exec(trimmed);
  if (fromPath?.[1]) return fromPath[1];
  const any = /\b(pr[a-f0-9]{64})\b/.exec(trimmed);
  return any?.[1] ?? null;
}
