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

/** Hosted Vercel Functions (OIDC gateway auth available without an API key). */
export function isVercelHosted(): boolean {
  return (
    process.env.VERCEL === "1" ||
    Boolean(process.env.VERCEL_DEPLOYMENT_ID?.trim()) ||
    Boolean(process.env.VERCEL_ENV?.trim())
  );
}

/** Whether `resolveGatewayModel` can run (API key, alias, or Vercel OIDC). */
export function isGatewayConfigured(): boolean {
  ensureGatewayApiKey();
  if (Boolean(process.env.AI_GATEWAY_API_KEY?.trim())) return true;
  return isVercelHosted();
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
  if (!isGatewayConfigured()) {
    throw new Error(
      [
        "Vercel AI Gateway is not configured.",
        "On Vercel: link the project — gateway model ids authenticate via OIDC (no API key).",
        "Off Vercel: set AI_GATEWAY_API_KEY or run `plasm-agent link`.",
      ].join(" "),
    );
  }

  // On Vercel without an explicit key, @ai-sdk/gateway uses @vercel/oidc automatically.
  return gateway(slug);
}
