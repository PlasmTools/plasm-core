import { randomUUID } from "node:crypto";

import { SpanStatusCode, trace } from "@opentelemetry/api";
import type { TelemetryOptions } from "ai";

import { frameworkPackageVersion } from "../package-version.js";

const EVE_TRACER_NAME = "eve";

export type EveChannelKind = string;

export interface EveEmissionState {
  sessionId: string;
  turnId: string;
  sequence: number;
  stepIndex: number;
  channelKind: EveChannelKind;
}

function eveEnvironment(): string {
  return process.env.VERCEL_ENV?.trim() || process.env.NODE_ENV?.trim() || "development";
}

/** Session id for one agent turn — matches Eve `eve.session.id` on Agent Runs spans. */
export function createEveSessionId(): string {
  return randomUUID();
}

/** Turn id inside a session (Eve default: `turn_0`, `turn_1`, …). */
export function createEveTurnId(sequence: number): string {
  return `turn_${sequence}`;
}

/** Runtime context keys Vercel Agent Runs reads from AI SDK spans. */
export function buildEveRuntimeContext(state: EveEmissionState): Record<string, string> {
  return {
    "eve.version": frameworkPackageVersion(),
    "eve.environment": eveEnvironment(),
    "eve.session.id": state.sessionId,
    "eve.turn.id": state.turnId,
    "eve.turn.sequence": String(state.sequence),
    "eve.step.index": String(state.stepIndex),
    "eve.channel.kind": state.channelKind,
  };
}

/** Eve-style telemetry options: record I/O + propagate runtime context onto spans. */
export function enrichEveTelemetry(
  base: TelemetryOptions,
  runtimeContext: Record<string, string>,
): TelemetryOptions {
  const includeRuntimeContext: Record<string, boolean> = {};
  for (const key of Object.keys(runtimeContext)) {
    includeRuntimeContext[key] = true;
  }
  return {
    ...base,
    isEnabled: base.isEnabled ?? true,
    recordInputs: base.recordInputs ?? true,
    recordOutputs: base.recordOutputs ?? true,
    includeRuntimeContext,
  };
}

export interface EveTurnSpanOptions extends EveEmissionState {
  functionId?: string;
  attributes?: Record<string, string | number | boolean>;
}

function eveTurnSpanAttributes(options: EveTurnSpanOptions): Record<string, string | number | boolean> {
  return {
    "eve.version": frameworkPackageVersion(),
    "eve.environment": eveEnvironment(),
    "eve.session.id": options.sessionId,
    "eve.turn.id": options.turnId,
    "eve.turn.sequence": options.sequence,
    "eve.step.index": options.stepIndex,
    "eve.channel.kind": options.channelKind,
    ...(options.functionId ? { "ai.telemetry.functionId": options.functionId } : {}),
    ...options.attributes,
  };
}

/**
 * Parent span Vercel Agent Runs ingests (`ai.eve.turn` + `eve.session.id`).
 * Eve emits one per tool-loop step; Plasm matches that shape.
 */
export async function withEveTurnSpan<T>(
  options: EveTurnSpanOptions,
  fn: () => Promise<T>,
): Promise<T> {
  const tracer = trace.getTracer(EVE_TRACER_NAME);
  const attributes = eveTurnSpanAttributes(options);

  return tracer.startActiveSpan("ai.eve.turn", { attributes }, async (span) => {
    try {
      return await fn();
    } catch (err) {
      span.setStatus({ code: SpanStatusCode.ERROR, message: String(err) });
      throw err;
    }
  });
}
