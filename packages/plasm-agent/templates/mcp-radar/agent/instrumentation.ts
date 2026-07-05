import path from "node:path";
import { fileURLToPath } from "node:url";

import { OpenTelemetry } from "@ai-sdk/otel";
import { registerOTel } from "@vercel/otel";
import { registerTelemetry } from "ai";

const agentRoot = path.dirname(fileURLToPath(import.meta.url));
const serviceName =
  process.env.PLASM_AGENT_NAME?.trim() || path.basename(path.dirname(agentRoot));

export function register(): void {
  registerOTel({ serviceName });
  registerTelemetry(new OpenTelemetry({ runtimeContext: true }));
}

register();
