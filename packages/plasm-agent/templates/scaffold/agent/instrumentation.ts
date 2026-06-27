import path from "node:path";
import { fileURLToPath } from "node:url";

import { registerOTel } from "@vercel/otel";
import { OpenTelemetry } from "@ai-sdk/otel";
import { registerTelemetryIntegration } from "ai";

const agentRoot = path.dirname(fileURLToPath(import.meta.url));
const serviceName =
  process.env.PLASM_AGENT_NAME?.trim() || path.basename(path.dirname(agentRoot));

/** Eve-shaped OTEL bootstrap — optional export to Braintrust/Datadog via registerOTel. */
export function register(): void {
  registerOTel({ serviceName });
  registerTelemetryIntegration(new OpenTelemetry());
}

register();
