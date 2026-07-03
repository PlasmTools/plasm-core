function workflowDispatchUrl(): string {
  const explicit = process.env.PLASM_WORKFLOW_DISPATCH_URL?.trim();
  if (explicit) return explicit;
  const production = process.env.VERCEL_PROJECT_PRODUCTION_URL?.trim();
  if (production) return `https://${production}/internal/workflow/dispatch`;
  const vercelUrl = process.env.VERCEL_URL?.trim();
  if (vercelUrl) return `https://${vercelUrl}/internal/workflow/dispatch`;
  const port = process.env.PORT ?? "3000";
  return `http://127.0.0.1:${port}/internal/workflow/dispatch`;
}

export async function mcpRadarScanWorkflow(_agentRoot: string, force = false, reset = false) {
  "use workflow";
  return mcpRadarScanStep(force, reset);
}

async function mcpRadarScanStep(force: boolean, reset: boolean) {
  "use step";
  const response = await fetch(workflowDispatchUrl(), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ job: "mcp-radar-scan", force, reset }),
  });
  if (!response.ok) {
    throw new Error(`workflow dispatch failed: ${response.status} ${await response.text()}`);
  }
  return response.json();
}
