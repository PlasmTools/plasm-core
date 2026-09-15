/** Default per-generation ceiling when `PLASM_EVAL_MAX_OUTPUT_TOKENS` is unset. */
export const DEFAULT_EVAL_MAX_OUTPUT_TOKENS = 16_384;

/**
 * Resolve per-generation `maxOutputTokens` from an env raw string.
 *
 * - **Unset** (`undefined`) → `defaultValue` (16384).
 * - **Set but invalid** (empty, non-integer, ≤0, fractional, NaN) → hard error.
 *   Never silently substitute the default for an explicit bad value.
 */
export function resolveEvalMaxOutputTokens(
  raw: string | undefined,
  defaultValue: number = DEFAULT_EVAL_MAX_OUTPUT_TOKENS,
): number {
  if (raw === undefined) return defaultValue;
  const trimmed = raw.trim();
  const parsed = trimmed === "" ? Number.NaN : Number(trimmed);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(
      `PLASM_EVAL_MAX_OUTPUT_TOKENS is set but invalid (${JSON.stringify(raw)}); ` +
        `expected a positive integer. Omit the variable to use the default (${defaultValue}).`,
    );
  }
  return parsed;
}
