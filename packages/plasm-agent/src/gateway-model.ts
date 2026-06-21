import { gateway, type LanguageModel } from "ai";

import type { AgentModelOptions } from "./define-agent.js";

function ensureGatewayApiKey(): void {
  if (process.env.AI_GATEWAY_API_KEY?.trim()) return;
  const alias =
    process.env.AI_API_GATEWAY_KEY?.trim() ?? process.env.AI_GATEWAY_KEY?.trim();
  if (alias) {
    process.env.AI_GATEWAY_API_KEY = alias;
  }
}

/** Whether `resolveGatewayModel` can run (API key or mapped alias present). */
export function isGatewayConfigured(): boolean {
  ensureGatewayApiKey();
  return Boolean(process.env.AI_GATEWAY_API_KEY?.trim());
}

/** Resolve a Gateway model slug (`provider/model`) to an AI SDK `LanguageModel`. */
export function resolveGatewayModel(
  model: string | LanguageModel,
  _options?: AgentModelOptions,
): LanguageModel {
  if (typeof model !== "string") return model;
  const slug = model.trim();
  if (!slug) {
    throw new Error("agent `model` must be a non-empty AI Gateway slug");
  }

  ensureGatewayApiKey();
  if (!process.env.AI_GATEWAY_API_KEY?.trim()) {
    throw new Error(
      [
        "Vercel AI Gateway requires AI_GATEWAY_API_KEY.",
        "Create one in the Vercel dashboard (AI → Gateway → API Keys) and add it to plasm-oss/.env.",
        "Local dev: `vercel env pull` from a linked project also works.",
      ].join(" "),
    );
  }

  return gateway(slug);
}
