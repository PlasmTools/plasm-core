import { registerOTel } from "@vercel/otel";
import { OpenTelemetry } from "@ai-sdk/otel";
import { registerTelemetry } from "ai";

export function register(): void {
  registerOTel({ serviceName: "plasm-agent" });
  registerTelemetry(new OpenTelemetry());
}

register();
