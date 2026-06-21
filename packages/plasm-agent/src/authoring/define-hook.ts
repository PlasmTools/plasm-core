import type { AuthoringContext } from "./context.js";

export const PLASM_HOOK_KIND = "hook" as const;

export type HookEvent = "agent:start" | "agent:step" | "plan:commit" | "run:complete";

export type HookHandler = (
  ctx: AuthoringContext,
  detail?: Record<string, unknown>,
) => void | Promise<void>;

export interface HookDefinition {
  readonly __plasmSlotKind: typeof PLASM_HOOK_KIND;
  name: string;
  on: HookEvent | HookEvent[];
  handler: HookHandler;
}

export interface DefineHookInput {
  name: string;
  on: HookEvent | HookEvent[];
  handler: HookHandler;
}

/** Lifecycle hook (trace/archive side effects). */
export function defineHook(input: DefineHookInput): HookDefinition {
  if (!input.name?.trim()) {
    throw new Error("defineHook: name is required");
  }
  const events = Array.isArray(input.on) ? input.on : [input.on];
  if (!events.length) {
    throw new Error("defineHook: on requires at least one event");
  }
  if (typeof input.handler !== "function") {
    throw new Error("defineHook: handler must be a function");
  }
  return Object.freeze({
    __plasmSlotKind: PLASM_HOOK_KIND,
    name: input.name.trim(),
    on: input.on,
    handler: input.handler,
  });
}

export function isHookDefinition(value: unknown): value is HookDefinition {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as HookDefinition).__plasmSlotKind === PLASM_HOOK_KIND
  );
}
