import { randomUUID } from "node:crypto";

import { context, SpanStatusCode, trace } from "@opentelemetry/api";

import { frameworkPackageVersion } from "../package-version.js";

const EVE_TRACER_NAME = "eve";

function eveEnvironment(): string {
  return process.env.VERCEL_ENV?.trim() || process.env.NODE_ENV?.trim() || "development";
}

/** Session id for one agent turn — matches Eve `eve.session.id` on Agent Runs spans. */
export function createEveSessionId(): string {
  return randomUUID();
}

/**
 * Parent span Vercel Agent Runs ingests (`ai.eve.turn` + `eve.session.id`).
 * Eve tool-loop emits one per step; Plasm wraps a full `generateText` turn.
 */
export async function withEveTurnSpan<T>(
  options: {
    sessionId: string;
    functionId?: string;
    attributes?: Record<string, string | number | boolean>;
  },
  fn: () => Promise<T>,
): Promise<T> {
  const tracer = trace.getTracer(EVE_TRACER_NAME);
  const attributes: Record<string, string | number | boolean> = {
    "eve.version": frameworkPackageVersion(),
    "eve.environment": eveEnvironment(),
    "eve.session.id": options.sessionId,
    ...options.attributes,
  };
  if (options.functionId) {
    attributes["ai.telemetry.functionId"] = options.functionId;
  }

  return tracer.startActiveSpan("ai.eve.turn", { attributes }, async (span) => {
    try {
      return await context.with(trace.setSpan(context.active(), span), fn);
    } catch (err) {
      span.setStatus({ code: SpanStatusCode.ERROR, message: String(err) });
      throw err;
    } finally {
      span.end();
    }
  });
}
