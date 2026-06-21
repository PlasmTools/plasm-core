import type { ProjectInfoPayload } from "../../project-info.js";
import { ANSI, paint, wrap } from "./ansi.js";
import type { DevSessionRef } from "./http-session.js";

export interface SlashContext {
  info: ProjectInfoPayload | null;
  session: DevSessionRef | null;
  baseUrl: string;
}

export type SlashResult =
  | { kind: "continue" }
  | { kind: "clear_session" }
  | { kind: "quit" }
  | { kind: "print"; lines: string[] };

const HELP_LINES = [
  "Slash commands:",
  "  /help       this list",
  "  /info       project + catalog summary",
  "  /model      current agent model",
  "  /channels   HTTP channel routes",
  "  /catalogs   loaded catalogs",
  "  /new        start a fresh session",
  "  /quit       exit",
];

export function handleSlashCommand(input: string, ctx: SlashContext): SlashResult {
  const cmd = input.trim().toLowerCase();
  if (cmd === "/help" || cmd === "/?") {
    return { kind: "print", lines: HELP_LINES };
  }
  if (cmd === "/quit" || cmd === "/exit" || cmd === "/q") {
    return { kind: "quit" };
  }
  if (cmd === "/new") {
    return { kind: "clear_session" };
  }
  if (cmd === "/info") {
    if (!ctx.info) return { kind: "print", lines: ["info unavailable — server not reachable"] };
    const i = ctx.info;
    const lines = [
      `${i.framework}  status=${i.status}`,
      `project: ${i.projectRoot}`,
      `gateway: ${i.gateway.configured ? "configured" : "missing"}`,
      `catalogs: ${i.discovery.catalogs.length}  channels: ${i.discovery.channels.length}`,
      `skills: ${i.discovery.skills.length}  subagents: ${i.loadedSlots.subagents.length}`,
    ];
    if (ctx.session) {
      lines.push(`session: ${ctx.session.sessionId.slice(0, 8)}…`);
    }
    return { kind: "print", lines };
  }
  if (cmd === "/model") {
    const model = ctx.info?.dev?.model ?? "(unknown — start dev server or /info)";
    return { kind: "print", lines: [`model: ${String(model)}`] };
  }
  if (cmd === "/channels") {
    const routes = ctx.info?.dev?.routes.channels ?? [];
    if (!routes.length) return { kind: "print", lines: ["no channels loaded"] };
    return {
      kind: "print",
      lines: routes.map((r) => `  ${r.method} ${r.path}`),
    };
  }
  if (cmd === "/catalogs") {
    const catalogs = ctx.info?.catalogs ?? [];
    if (!catalogs.length) return { kind: "print", lines: ["no catalogs"] };
    return {
      kind: "print",
      lines: catalogs.map((c) => {
        const fresh = c.stubFresh?.fresh ? "fresh" : c.stubFresh ? "stale" : "missing";
        return `  ${c.name}  stub=${fresh}`;
      }),
    };
  }
  return { kind: "print", lines: [`unknown command ${cmd} — try /help`] };
}

export function printBanner(baseUrl: string, info: ProjectInfoPayload | null): void {
  const title = paint("plasm-agent dev", ANSI.bold, ANSI.cyan);
  const url = paint(baseUrl, ANSI.dim);
  console.log(`\n${title}  ${url}`);
  if (info?.dev?.model) {
    console.log(paint(`model: ${String(info.dev.model)}`, ANSI.dim));
  }
  console.log(paint("Type a message or /help.  /quit to exit.", ANSI.dim));
  console.log("");
}

export function printUserLine(text: string): void {
  console.log(paint("you › ", ANSI.bold, ANSI.green) + text);
}

export function printAssistant(text: string): void {
  const prefix = paint("agent › ", ANSI.bold, ANSI.magenta);
  for (const line of wrap(text)) {
    console.log(prefix + line);
  }
}

export function printMeta(text: string): void {
  console.log(paint(text, ANSI.dim));
}

export function printError(text: string): void {
  console.log(paint(`error: ${text}`, ANSI.red));
}

export function printStep(tools: string[]): void {
  if (!tools.length) return;
  console.log(paint(`  · tools: ${tools.join(", ")}`, ANSI.blue));
}

export function printLines(lines: string[]): void {
  for (const line of lines) {
    console.log(paint(line, ANSI.yellow));
  }
}
