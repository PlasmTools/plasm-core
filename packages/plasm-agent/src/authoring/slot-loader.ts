import { access, readFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

import type { ProjectDiscovery, DiscoveryDiagnostic } from "../discovery/project-walker.js";
import type { ChannelDefinition } from "./define-channel.js";
import { isChannelDefinition } from "./define-channel.js";
import type { HookDefinition } from "./define-hook.js";
import { isHookDefinition } from "./define-hook.js";
import type { ScheduleDefinition } from "./define-schedule.js";
import { isScheduleDefinition } from "./define-schedule.js";
import type { SkillDefinition } from "./define-skill.js";
import { isSkillDefinition } from "./define-skill.js";
import { HookRunner } from "./hook-runner.js";

export interface LoadedChannel {
  sourcePath: string;
  definition: ChannelDefinition;
}

export interface LoadedSchedule {
  sourcePath: string;
  definition: ScheduleDefinition;
}

export interface LoadedHook {
  sourcePath: string;
  definition: HookDefinition;
}

export interface LoadedSkill {
  sourcePath: string;
  definition: SkillDefinition;
}

export interface LoadedProjectSlots {
  channels: LoadedChannel[];
  schedules: LoadedSchedule[];
  hooks: LoadedHook[];
  skills: LoadedSkill[];
  hookRunner: HookRunner;
  diagnostics: DiscoveryDiagnostic[];
}

export interface LoadAuthoredSlotsOptions {
  discovery: ProjectDiscovery;
  importCacheBust?: number;
  agentRoot?: string;
  projectRoot?: string;
  /** agentRoot-relative source → projectRoot-relative compiled `.mjs` from build manifest. */
  compiledSlots?: Record<string, string>;
}

type SlotName = "channels" | "schedules" | "hooks" | "skills";

function slotDiagnostic(
  slot: SlotName,
  filePath: string,
  message: string,
  level: DiscoveryDiagnostic["level"] = "error",
): DiscoveryDiagnostic {
  return { level, slot, path: filePath, message };
}

async function pathExists(p: string): Promise<boolean> {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

async function resolveSlotImportPath(
  filePath: string,
  options: LoadAuthoredSlotsOptions,
): Promise<string> {
  const agentRoot = path.resolve(options.agentRoot ?? options.discovery.agentRoot);
  const relFromAgent = path.relative(agentRoot, filePath);
  const fromManifest = options.compiledSlots?.[relFromAgent];
  if (fromManifest && options.projectRoot) {
    const compiled = path.join(options.projectRoot, fromManifest);
    if (await pathExists(compiled)) return compiled;
  }
  const mirror = path.join(
    agentRoot,
    ".plasm",
    "compiled",
    relFromAgent.replace(/\.ts$/, ".mjs"),
  );
  if (await pathExists(mirror)) return mirror;
  return filePath;
}

async function importSlotModule(
  filePath: string,
  cacheBust: number,
  options: LoadAuthoredSlotsOptions,
): Promise<unknown> {
  const agentRoot = path.resolve(options.agentRoot ?? options.discovery.agentRoot);
  const relFromAgent = path.relative(agentRoot, filePath);
  const preloaded = (
    globalThis as typeof globalThis & { __PLASM_PRELOADED_SLOTS?: Record<string, unknown> }
  ).__PLASM_PRELOADED_SLOTS?.[relFromAgent];
  if (preloaded !== undefined) {
    return (preloaded as { default?: unknown }).default ?? preloaded;
  }

  const resolved = await resolveSlotImportPath(filePath, options);
  const url = `${pathToFileURL(resolved).href}?t=${cacheBust}`;
  const mod = await import(url);
  return mod.default ?? mod;
}

function classifySlotExport(
  exported: unknown,
): "channel" | "schedule" | "hook" | "skill" | null {
  if (isChannelDefinition(exported)) return "channel";
  if (isScheduleDefinition(exported)) return "schedule";
  if (isHookDefinition(exported)) return "hook";
  if (isSkillDefinition(exported)) return "skill";
  return null;
}

const EXPECTED_KIND: Record<Exclude<SlotName, "skills">, "channel" | "schedule" | "hook"> = {
  channels: "channel",
  schedules: "schedule",
  hooks: "hook",
};

async function loadTypescriptSlot<T extends ChannelDefinition | ScheduleDefinition | HookDefinition | SkillDefinition>(
  filePath: string,
  slot: SlotName,
  expected: "channel" | "schedule" | "hook" | "skill",
  cacheBust: number,
  options: LoadAuthoredSlotsOptions,
): Promise<{ definition: T } | { diagnostic: DiscoveryDiagnostic }> {
  try {
    const exported = await importSlotModule(filePath, cacheBust, options);
    const actual = classifySlotExport(exported);
    if (actual !== expected) {
      const helper =
        expected === "channel"
          ? "defineChannel"
          : expected === "schedule"
            ? "defineSchedule"
            : expected === "hook"
              ? "defineHook"
              : "defineSkill";
      return {
        diagnostic: slotDiagnostic(
          slot,
          filePath,
          `default export must be ${helper}() result`,
        ),
      };
    }
    return { definition: exported as T };
  } catch (err) {
    return {
      diagnostic: slotDiagnostic(slot, filePath, `failed to import: ${String(err)}`),
    };
  }
}

async function loadMarkdownSkill(filePath: string, name: string): Promise<LoadedSkill> {
  const body = await readFile(filePath, "utf8");
  const firstLine = body.split("\n").find((line) => line.trim())?.trim() ?? "";
  const description = firstLine.startsWith("#")
    ? firstLine.replace(/^#+\s*/, "").trim()
    : undefined;
  return {
    sourcePath: filePath,
    definition: {
      __plasmSlotKind: "skill",
      name,
      description,
      body,
    },
  };
}

/** Import authored slots from discovery metadata. */
export async function loadAuthoredSlots(
  options: LoadAuthoredSlotsOptions,
): Promise<LoadedProjectSlots> {
  const { discovery } = options;
  const cacheBust = options.importCacheBust ?? Date.now();
  const diagnostics: DiscoveryDiagnostic[] = [...discovery.diagnostics];

  const channels: LoadedChannel[] = [];
  for (const file of discovery.channels) {
    if (file.kind !== "typescript") continue;
    const result = await loadTypescriptSlot<ChannelDefinition>(
      file.path,
      "channels",
      EXPECTED_KIND.channels,
      cacheBust,
      options,
    );
    if ("diagnostic" in result) {
      diagnostics.push(result.diagnostic);
      continue;
    }
    channels.push({ sourcePath: file.path, definition: result.definition });
  }

  const schedules: LoadedSchedule[] = [];
  for (const file of discovery.schedules) {
    if (file.kind !== "typescript") continue;
    const result = await loadTypescriptSlot<ScheduleDefinition>(
      file.path,
      "schedules",
      EXPECTED_KIND.schedules,
      cacheBust,
      options,
    );
    if ("diagnostic" in result) {
      diagnostics.push(result.diagnostic);
      continue;
    }
    schedules.push({ sourcePath: file.path, definition: result.definition });
  }

  const hooks: LoadedHook[] = [];
  for (const file of discovery.hooks) {
    if (file.kind !== "typescript") continue;
    const result = await loadTypescriptSlot<HookDefinition>(
      file.path,
      "hooks",
      EXPECTED_KIND.hooks,
      cacheBust,
      options,
    );
    if ("diagnostic" in result) {
      diagnostics.push(result.diagnostic);
      continue;
    }
    hooks.push({ sourcePath: file.path, definition: result.definition });
  }

  const skills: LoadedSkill[] = [];
  for (const file of discovery.skills) {
    if (file.kind === "markdown") {
      skills.push(await loadMarkdownSkill(file.path, file.name));
      continue;
    }
    const result = await loadTypescriptSlot<SkillDefinition>(
      file.path,
      "skills",
      "skill",
      cacheBust,
      options,
    );
    if ("diagnostic" in result) {
      diagnostics.push(result.diagnostic);
      continue;
    }
    skills.push({ sourcePath: file.path, definition: result.definition });
  }

  return {
    channels,
    schedules,
    hooks,
    skills,
    hookRunner: new HookRunner(hooks.map((h) => h.definition)),
    diagnostics,
  };
}

export interface LoadedSlotsSummary {
  skills: Array<{ name: string; path: string; kind: "markdown" | "typescript" }>;
  channels: Array<{ name: string; path: string; routes: Array<{ method: string; path: string }> }>;
  schedules: Array<{ name: string; path: string; cron: string }>;
  hooks: Array<{ name: string; path: string; on: string | string[] }>;
}

export function summarizeLoadedSlots(
  slots: LoadedProjectSlots,
  agentRoot: string,
): LoadedSlotsSummary {
  const rel = (p: string) => path.relative(agentRoot, p);
  return {
    skills: slots.skills.map((s) => ({
      name: s.definition.name,
      path: rel(s.sourcePath),
      kind: s.sourcePath.endsWith(".md") ? "markdown" : "typescript",
    })),
    channels: slots.channels.map((c) => ({
      name: c.definition.name,
      path: rel(c.sourcePath),
      routes: c.definition.routes.map((r) => ({ method: r.method, path: r.path })),
    })),
    schedules: slots.schedules.map((s) => ({
      name: s.definition.name,
      path: rel(s.sourcePath),
      cron: s.definition.cron,
    })),
    hooks: slots.hooks.map((h) => ({
      name: h.definition.name,
      path: rel(h.sourcePath),
      on: h.definition.on,
    })),
  };
}
