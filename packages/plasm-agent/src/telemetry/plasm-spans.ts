import { SpanStatusCode, trace, type Span } from "@opentelemetry/api";

import { PlasmSpanAttributes } from "../instrumentation.js";

export interface PlasmSpanContext {
  sessionId?: string;
  intent?: string;
  logicalSessionRef?: string;
  catalogCgsHash?: string;
  entryId?: string;
  runRef?: string;
  runId?: string;
  toolName?: string;
  transportHost?: string;
  transportStatus?: number;
}

const tracer = trace.getTracer("plasm-agent");

function applyPlasmAttributes(span: Span, ctx: PlasmSpanContext): void {
  if (ctx.sessionId) span.setAttribute(PlasmSpanAttributes.SESSION_ID, ctx.sessionId);
  if (ctx.intent) span.setAttribute(PlasmSpanAttributes.INTENT, ctx.intent);
  if (ctx.logicalSessionRef) {
    span.setAttribute(PlasmSpanAttributes.LOGICAL_SESSION_REF, ctx.logicalSessionRef);
  }
  if (ctx.catalogCgsHash) {
    span.setAttribute(PlasmSpanAttributes.CATALOG_CGS_HASH, ctx.catalogCgsHash);
  }
  if (ctx.runRef) {
    span.setAttribute(PlasmSpanAttributes.RUN_REF, ctx.runRef);
  }
  if (ctx.runId) span.setAttribute(PlasmSpanAttributes.RUN_ID, ctx.runId);
  if (ctx.entryId) span.setAttribute(PlasmSpanAttributes.ENTRY_ID, ctx.entryId);
  if (ctx.toolName) span.setAttribute(PlasmSpanAttributes.TOOL_NAME, ctx.toolName);
  if (ctx.transportHost) {
    span.setAttribute(PlasmSpanAttributes.TRANSPORT_HOST, ctx.transportHost);
  }
  if (ctx.transportStatus !== undefined) {
    span.setAttribute(PlasmSpanAttributes.TRANSPORT_STATUS, ctx.transportStatus);
  }
}

export async function withPlasmSpan<T>(
  name: string,
  ctx: PlasmSpanContext,
  fn: (span: Span) => Promise<T>,
): Promise<T> {
  return tracer.startActiveSpan(name, async (span) => {
    applyPlasmAttributes(span, ctx);
    try {
      const result = await fn(span);
      span.setStatus({ code: SpanStatusCode.OK });
      return result;
    } catch (error) {
      span.recordException(error as Error);
      span.setStatus({ code: SpanStatusCode.ERROR });
      throw error;
    } finally {
      span.end();
    }
  });
}

export const plasmSpans = {
  toolDiscover: <T>(ctx: PlasmSpanContext, fn: (span: Span) => Promise<T>) =>
    withPlasmSpan("tool.discover_capabilities", { ...ctx, toolName: "discover_capabilities" }, fn),

  toolContext: <T>(ctx: PlasmSpanContext, fn: (span: Span) => Promise<T>) =>
    withPlasmSpan("tool.plasm_context", { ...ctx, toolName: "plasm_context" }, fn),

  dryRun: <T>(ctx: PlasmSpanContext, fn: (span: Span) => Promise<T>) =>
    withPlasmSpan("plasm.dry_run", ctx, fn),

  liveRun: <T>(ctx: PlasmSpanContext, fn: (span: Span) => Promise<T>) =>
    withPlasmSpan("plasm.live_run", ctx, fn),

  transportHttp: <T>(ctx: PlasmSpanContext, fn: (span: Span) => Promise<T>) =>
    withPlasmSpan("plasm.transport.http", ctx, fn),
};

export function activeTraceId(): string | undefined {
  return trace.getActiveSpan()?.spanContext().traceId;
}
