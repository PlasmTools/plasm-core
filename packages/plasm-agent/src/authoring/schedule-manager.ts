import type { IncomingMessage, ServerResponse } from "node:http";

import type { AuthoringContext } from "./context.js";
import { cronIntervalMs } from "./define-schedule.js";
import type { LoadedSchedule } from "./slot-loader.js";
import { resolveWorkflowWorldType } from "../workflow/world-bootstrap.js";
import type { AgentWorkflowDefinition } from "../define-agent.js";

export interface ScheduleHandle {
  stop(): void;
}

export interface ScheduleTaskManifest {
  scheduledTasks: Record<string, string | string[]>;
  tasks: Array<{ name: string; cron: string }>;
}

export interface ScheduleCronManifest {
  crons: Array<{ path: string; schedule: string; name: string }>;
}

export function exportScheduleTaskManifest(schedules: LoadedSchedule[]): ScheduleTaskManifest {
  const scheduledTasks: Record<string, string | string[]> = {};
  const tasks: Array<{ name: string; cron: string }> = [];

  for (const schedule of schedules) {
    const { name, cron } = schedule.definition;
    tasks.push({ name, cron });
    const existing = scheduledTasks[cron];
    if (!existing) {
      scheduledTasks[cron] = name;
    } else if (Array.isArray(existing)) {
      existing.push(name);
    } else {
      scheduledTasks[cron] = [existing, name];
    }
  }

  return { scheduledTasks, tasks };
}

/** @deprecated Use Nitro scheduled tasks in production. */
export function exportScheduleCronManifest(schedules: LoadedSchedule[]) {
  return {
    crons: schedules.map((schedule) => ({
      name: schedule.definition.name,
      path: `/internal/schedule/${schedule.definition.name}`,
      schedule: schedule.definition.cron,
    })),
  };
}

/** Eve-style dev dispatch — manually trigger a schedule by name. */
export function tryHandleScheduleDevDispatch(
  req: IncomingMessage,
  res: ServerResponse,
  schedules: LoadedSchedule[],
  ctx: AuthoringContext,
): boolean {
  const method = (req.method ?? "GET").toUpperCase();
  const pathname = new URL(req.url ?? "/", "http://localhost").pathname;
  const match = pathname.match(/^\/internal\/schedule\/([^/]+)$/);
  if (method !== "POST" || !match) return false;

  const name = match[1]!;
  const schedule = schedules.find((s) => s.definition.name === name);
  if (!schedule) {
    res.statusCode = 404;
    res.setHeader("content-type", "application/json; charset=utf-8");
    res.end(JSON.stringify({ error: "schedule_not_found", name }));
    return true;
  }

  void Promise.resolve(schedule.definition.handler(ctx))
    .then(() => {
      res.statusCode = 200;
      res.setHeader("content-type", "application/json; charset=utf-8");
      res.end(JSON.stringify({ ok: true, schedule: name, mode: "dev_dispatch" }));
    })
    .catch((err: unknown) => {
      res.statusCode = 500;
      res.setHeader("content-type", "application/json; charset=utf-8");
      res.end(JSON.stringify({ error: "schedule_failed", message: String(err) }));
    });

  return true;
}

export function startScheduleTimers(
  schedules: LoadedSchedule[],
  ctx: AuthoringContext,
  workflow?: AgentWorkflowDefinition,
): ScheduleHandle {
  const world = resolveWorkflowWorldType(workflow);
  if (world !== "local") {
    const manifest = exportScheduleTaskManifest(schedules);
    console.log(
      `[plasm:schedule] prod world=${world} — Nitro scheduled tasks:`,
      JSON.stringify(manifest.scheduledTasks, null, 2),
    );
    return { stop: () => {} };
  }

  const timers: ReturnType<typeof setInterval>[] = [];

  for (const schedule of schedules) {
    const intervalMs = cronIntervalMs(schedule.definition.cron);
    if (!intervalMs) {
      console.warn(
        `[plasm:dev] schedule ${schedule.definition.name}: unsupported cron "${schedule.definition.cron}" (use */N * * * *)`,
      );
      continue;
    }

    const run = () => {
      void Promise.resolve(schedule.definition.handler(ctx)).catch((err: unknown) => {
        console.error(`[plasm:dev] schedule ${schedule.definition.name} failed:`, err);
      });
    };

    timers.push(setInterval(run, intervalMs));
    console.log(
      `[plasm:dev] schedule ${schedule.definition.name} every ${intervalMs / 60_000}m (${schedule.definition.cron})`,
    );
  }

  return {
    stop: () => {
      for (const timer of timers) clearInterval(timer);
    },
  };
}
