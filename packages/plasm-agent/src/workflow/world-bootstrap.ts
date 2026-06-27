import type { AgentWorkflowDefinition } from "../define-agent.js";

export type WorkflowWorldType = "local" | "postgres" | "vercel";

export function resolveWorkflowWorldType(
  definition?: AgentWorkflowDefinition,
): WorkflowWorldType {
  const fromDef = definition?.world?.type;
  if (fromDef === "local" || fromDef === "postgres" || fromDef === "vercel") {
    return fromDef;
  }
  const env = process.env.PLASM_WORKFLOW_WORLD?.trim().toLowerCase();
  if (env === "local" || env === "postgres" || env === "vercel") {
    return env;
  }
  if (process.env.VERCEL === "1") return "vercel";
  return "local";
}

/**
 * Bootstrap Workflow SDK world from agent definition.
 * - `vercel`: Vercel World is selected automatically by the Workflow SDK when
 *   `VERCEL_DEPLOYMENT_ID` is set; durable session routes require `/.well-known/workflow/`
 *   (eve emits these via Nitro + @workflow/nitro when sources contain `use workflow` / `use step`).
 * - `postgres`: `@workflow/world-postgres` long-lived worker
 * - `local`: `@workflow/world-local` for dev
 */
export async function bootstrapWorkflowWorld(
  definition?: AgentWorkflowDefinition,
): Promise<WorkflowWorldType> {
  const worldType = resolveWorkflowWorldType(definition);
  if (worldType === "vercel") {
    if (process.env.VERCEL_DEPLOYMENT_ID?.trim()) {
      try {
        const { getWorld } = await import("workflow/runtime");
        await getWorld().start?.();
      } catch (err) {
        if (process.env.PLASM_WORKFLOW_STRICT === "1") throw err;
        console.warn("[plasm:workflow] vercel world start skipped:", err);
      }
    }
    return worldType;
  }

  if (worldType === "postgres") {
    process.env.WORKFLOW_TARGET_WORLD ??= "@workflow/world-postgres";
    process.env.WORKFLOW_POSTGRES_URL ??=
      process.env.DATABASE_URL?.trim() ||
      process.env.PLASM_STATE_POSTGRES_URL?.trim();
  } else {
    process.env.WORKFLOW_TARGET_WORLD ??= "@workflow/world-local";
  }

  try {
    const { getWorld } = await import("workflow/runtime");
    const world = getWorld();
    await world.start?.();
  } catch (err) {
    if (process.env.PLASM_WORKFLOW_STRICT === "1") throw err;
  }

  return worldType;
}
