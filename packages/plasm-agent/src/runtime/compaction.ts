import { generateText, type LanguageModel, type ModelMessage } from "ai";

import type { AgentCompactionConfig } from "../define-agent.js";
import { resolveGatewayModel } from "../gateway-model.js";

const DEFAULT_CONTEXT_TOKENS = 200_000;

function estimateTokens(messages: ModelMessage[]): number {
  let chars = 0;
  for (const message of messages) {
    if (typeof message.content === "string") {
      chars += message.content.length;
    } else if (Array.isArray(message.content)) {
      for (const part of message.content) {
        if (typeof part === "object" && part && "text" in part && typeof part.text === "string") {
          chars += part.text.length;
        }
      }
    }
  }
  return Math.ceil(chars / 4);
}

function splitForCompaction(messages: ModelMessage[]): {
  prefix: ModelMessage[];
  suffix: ModelMessage[];
} {
  if (messages.length <= 4) {
    return { prefix: [], suffix: messages };
  }
  const keepRecent = Math.min(6, messages.length);
  return {
    prefix: messages.slice(0, messages.length - keepRecent),
    suffix: messages.slice(messages.length - keepRecent),
  };
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
