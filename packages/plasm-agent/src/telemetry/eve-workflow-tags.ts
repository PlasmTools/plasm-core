import type { LanguageModelUsage } from "ai";

import type { EveChannelKind } from "./eve-agent-runs.js";

export const EVE_SESSION_TITLE_MAX_CHARS = 125;

const EVE_TAG_VALUE_MAX_BYTES = 256;

function truncateForTag(value: string, maxBytes = EVE_TAG_VALUE_MAX_BYTES): string {
  if (maxBytes <= 0) return "";
  const encoder = new TextEncoder();
  if (encoder.encode(value).length <= maxBytes) return value;
  let end = value.length;
  while (end > 0) {
    const code = value.charCodeAt(end - 1);
    if (code >= 0xd800 && code <= 0xdbff) {
      end -= 1;
      continue;
    }
    const slice = value.slice(0, end);
    if (encoder.encode(slice).length <= maxBytes) return slice;
    end -= 1;
  }
  return "";
}

function deriveSessionTitle(input: string | undefined): string | undefined {
  if (!input?.trim()) return undefined;
  const normalized = input.replace(/\s+/gu, " ").trim();
  if (!normalized) return undefined;
  const chars = Array.from(normalized);
  if (chars.length <= EVE_SESSION_TITLE_MAX_CHARS) return normalized;
  return `${chars.slice(0, EVE_SESSION_TITLE_MAX_CHARS - 1).join("")}…`;
}

/** `$eve.*` tags for a root session workflow run (Agent Runs tree root). */
export function buildSessionAttributes(options: {
  channelKind?: EveChannelKind;
  title?: string;
}): Record<string, string> {
  const attrs: Record<string, string> = {
    "$eve.type": "session",
    "$eve.trigger": options.channelKind ?? "unknown",
  };
  const title = deriveSessionTitle(options.title);
  if (title) attrs["$eve.title"] = title;
  return attrs;
}

/** `$eve.*` tags for a turn workflow run (child of a session). */
export function buildTurnAttributes(options: {
  parentSessionId: string;
  rootSessionId: string;
}): Record<string, string> {
  return {
    "$eve.type": "turn",
    "$eve.parent": options.parentSessionId,
    "$eve.root": options.rootSessionId,
  };
}

/** Cumulative usage tags Eve writes on each turn step (last write wins). */
export function buildTurnUsageAttributes(options: {
  modelId?: string;
  usage?: LanguageModelUsage;
  toolCount?: number;
}): Record<string, string> {
  const attrs: Record<string, string> = {};
  if (options.modelId) attrs["$eve.model"] = options.modelId;
  if (options.usage) {
    attrs["$eve.input_tokens"] = String(options.usage.inputTokens ?? 0);
    attrs["$eve.output_tokens"] = String(options.usage.outputTokens ?? 0);
    const cacheRead = options.usage.inputTokenDetails?.cacheReadTokens;
    const cacheWrite = options.usage.inputTokenDetails?.cacheWriteTokens;
    if (cacheRead !== undefined) attrs["$eve.cache_read_tokens"] = String(cacheRead);
    if (cacheWrite !== undefined) attrs["$eve.cache_write_tokens"] = String(cacheWrite);
  }
  if (options.toolCount !== undefined) {
    attrs["$eve.tool_count"] = String(options.toolCount);
  }
  return attrs;
}

export type EveWorkflowAttributeInput = Record<string, string | number | boolean | undefined>;

export function stringifyEveWorkflowAttributes(
  attrs: EveWorkflowAttributeInput,
): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined) continue;
    out[key] = truncateForTag(typeof value === "string" ? value : String(value));
  }
  return out;
}
