import { LegacyOpenTelemetry } from "@ai-sdk/otel";
import {
  registerTelemetry,
  type Telemetry,
  type TelemetryOptions,
} from "ai";

/** OpenTelemetry attribute keys for Plasm agent spans (Section 9). */
export const PlasmSpanAttributes = {
  SESSION_ID: "plasm.session_id",
  INTENT: "plasm.intent",
  LOGICAL_SESSION_REF: "plasm.logical_session_ref",
  CATALOG_CGS_HASH: "plasm.catalog_cgs_hash",
  RUN_REF: "plasm.run_ref",
  RUN_ID: "plasm.run_id",
  ENTRY_ID: "plasm.entry_id",
  TOOL_NAME: "plasm.tool.name",
  TRANSPORT_HOST: "plasm.transport.host",
  TRANSPORT_STATUS: "plasm.transport.status",
} as const;

export interface AgentInstrumentationOptions {
  serviceName?: string;
}

let registered = false;

function otelIntegration(_options: AgentInstrumentationOptions): Telemetry {
  return new LegacyOpenTelemetry();
}

/** Register global AI SDK OTEL integration (eve-style auto-discovered instrumentation). */
export function registerAgentInstrumentation(
  options: AgentInstrumentationOptions = {},
): void {
  if (registered) return;
  registerTelemetry(otelIntegration(options));
  registered = true;
  void options.serviceName;
}

/** Per-call AI SDK telemetry settings (OTEL registered in agent/instrumentation.ts). */
export function createAgentTelemetry(
  options: AgentInstrumentationOptions = {},
): TelemetryOptions {
  return {
    isEnabled: true,
    functionId: options.serviceName ?? "plasm-agent",
  };
}
