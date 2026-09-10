import { generateText, type LanguageModel, type ModelMessage } from "ai";

import type { AgentCompactionConfig } from "../define-agent.js";
import { resolveGatewayModel } from "../gateway-model.js";

const DEFAULT_CONTEXT_TOKENS = 200_000;

function estimateTokens(messages: ModelMessage[]): number {
  return Math.ceil(JSON.stringify(messages).length / 4);
}

function splitForCompaction(messages: ModelMessage[]): {
  prefix: ModelMessage[];
  suffix: ModelMessage[];
} {
  if (messages.length <= 4) {
    return { prefix: [], suffix: messages };
  }
  const keepRecent = Math.min(6, messages.length);
  let split = messages.length - keepRecent;
  // Never separate a tool result from the assistant call that produced it.
  while (split > 0 && messages[split]?.role === "tool") split -= 1;
  return { prefix: messages.slice(0, split), suffix: messages.slice(split) };
}

/** Eve-shaped context trimming when transcript exceeds compaction threshold. */
export async function maybeCompactMessages(
  messages: ModelMessage[],
  compaction: AgentCompactionConfig | undefined,
  primaryModel: string | LanguageModel,
): Promise<ModelMessage[]> {
  if (!compaction?.thresholdPercent || messages.length < 3) {
    return messages;
  }

  const threshold = Math.floor(
    DEFAULT_CONTEXT_TOKENS * (compaction.thresholdPercent / 100),
  );
  const estimated = estimateTokens(messages);
  if (estimated < threshold) {
    return messages;
  }

  const { prefix, suffix } = splitForCompaction(messages);
  if (!prefix.length) {
    return messages;
  }

  const summaryModel = resolveGatewayModel(compaction.model ?? primaryModel);
  const transcript = prefix
    .map((m) => `${m.role}: ${typeof m.content === "string" ? m.content : JSON.stringify(m.content)}`)
    .join("\n\n");

  const summary = await generateText({
    model: summaryModel,
    system:
      "Summarize the prior agent conversation for continuation. Preserve goals, catalog picks, logical_session_ref, run_ref, and unresolved tasks. Be concise.",
    prompt: transcript,
    temperature: 0,
  });

  return [
    {
      role: "user",
      content: `[compacted context]\n${summary.text.trim()}`,
    },
    ...suffix,
  ];
}
