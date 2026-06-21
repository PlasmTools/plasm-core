import type { AuthoringContext } from "./context.js";

export const PLASM_SCHEDULE_KIND = "schedule" as const;

export type ScheduleHandler = (ctx: AuthoringContext) => void | Promise<void>;

export interface ScheduleDefinition {
  readonly __plasmSlotKind: typeof PLASM_SCHEDULE_KIND;
  name: string;
  cron: string;
  handler: ScheduleHandler;
}

export interface DefineScheduleInput {
  name: string;
  cron: string;
  handler: ScheduleHandler;
}

/** Cron schedule (Workflow-friendly; dev server uses a minimal every-N-minutes timer). */
export function defineSchedule(input: DefineScheduleInput): ScheduleDefinition {
  if (!input.name?.trim()) {
    throw new Error("defineSchedule: name is required");
  }
  if (!input.cron?.trim()) {
    throw new Error("defineSchedule: cron is required");
  }
  if (typeof input.handler !== "function") {
    throw new Error("defineSchedule: handler must be a function");
  }
  return Object.freeze({
    __plasmSlotKind: PLASM_SCHEDULE_KIND,
    name: input.name.trim(),
    cron: input.cron.trim(),
    handler: input.handler,
  });
}

export function isScheduleDefinition(value: unknown): value is ScheduleDefinition {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as ScheduleDefinition).__plasmSlotKind === PLASM_SCHEDULE_KIND
  );
}

/** Parse every-N-minutes or every-N-hours cron into interval ms; returns null when unsupported. */
export function cronIntervalMs(cron: string): number | null {
  const everyMinutes = cron.match(/^\*\/(\d+)\s+\*\s+\*\s+\*\s+\*$/);
  if (everyMinutes) {
    const minutes = Number(everyMinutes[1]);
    if (Number.isFinite(minutes) && minutes > 0) {
      return minutes * 60_000;
    }
  }
  const everyHours = cron.match(/^0\s+\*\/(\d+)\s+\*\s+\*\s+\*$/);
  if (everyHours) {
    const hours = Number(everyHours[1]);
    if (Number.isFinite(hours) && hours > 0) {
      return hours * 3_600_000;
    }
  }
  return null;
}
