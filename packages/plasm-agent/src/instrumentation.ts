import { LegacyOpenTelemetry } from "@ai-sdk/otel";
import type { AttributeValue } from "@opentelemetry/api";
import {
  registerTelemetryIntegration,
  type TelemetryIntegration,
  type TelemetrySettings,
} from "ai";

/** OpenTelemetry attribute keys for Plasm agent spans (Section 9). */
export const PlasmSpanAttributes = {
  SESSION_ID: "plasm.session_id",
  INTENT: "plasm.intent",
  LOGICAL_SESSION_REF: "plasm.logical_session_ref",
  CATALOG_CGS_HASH: "plasm.catalog_cgs_hash",
  PLAN_COMMIT_REF: "plasm.plan_commit_ref",
  RUN_ID: "plasm.run_id",
  ENTRY_ID: "plasm.entry_id",
  TOOL_NAME: "plasm.tool.name",
  TRANSPORT_HOST: "plasm.transport.host",
  TRANSPORT_STATUS: "plasm.transport.status",
} as const;

export interface AgentInstrumentationOptions {
  serviceName?: string;
  tracer?: TelemetrySettings["tracer"];
}

let registered = false;

function otelIntegration(options: AgentInstrumentationOptions): TelemetryIntegration {
  return new LegacyOpenTelemetry({ tracer: options.tracer }) as unknown as TelemetryIntegration;
}

/** Register global AI SDK OTEL integration (eve-style auto-discovered instrumentation). */
export function registerAgentInstrumentation(
  options: AgentInstrumentationOptions = {},
): void {
  if (registered) return;
  registerTelemetryIntegration(otelIntegration(options));
  registered = true;
  void options.serviceName;
}

/** Per-call AI SDK telemetry settings with OpenTelemetry integration. */
export function createAgentTelemetry(
  options: AgentInstrumentationOptions = {},
): TelemetrySettings {
  registerAgentInstrumentation(options);
  return {
    isEnabled: true,
    metadata: {
      "service.name": options.serviceName ?? "plasm-agent",
    } satisfies Record<string, AttributeValue>,
    integrations: otelIntegration(options),
  };
}
