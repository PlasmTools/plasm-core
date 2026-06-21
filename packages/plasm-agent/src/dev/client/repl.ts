import * as readline from "node:readline/promises";
import { stdin as input, stdout as output } from "node:process";

import { DevHttpSessionClient, type DevSessionRef } from "./http-session.js";
import {
  handleSlashCommand,
  printAssistant,
  printBanner,
  printError,
  printLines,
  printMeta,
  printStep,
  printUserLine,
  type SlashResult,
} from "./slash.js";
import type { ProjectInfoPayload } from "../../project-info.js";

export interface DevTuiOptions {
  baseUrl: string;
}

function applySlash(result: SlashResult): "quit" | "clear" | "handled" | "none" {
  if (result.kind === "quit") return "quit";
  if (result.kind === "clear_session") return "clear";
  if (result.kind === "print") {
    printLines(result.lines);
    return "handled";
  }
  return "none";
}

export async function runDevTui(options: DevTuiOptions): Promise<void> {
  const client = new DevHttpSessionClient(options.baseUrl);
  let info: ProjectInfoPayload | null = null;
  try {
    info = await client.fetchInfo();
  } catch (err) {
    printError(`could not reach ${options.baseUrl}: ${String(err)}`);
    return;
  }

  printBanner(options.baseUrl, info);
  let session: DevSessionRef | null = null;
  const rl = readline.createInterface({ input, output, terminal: true });

  try {
    while (true) {
      const line = (await rl.question("› ")).trim();
      if (!line) continue;

      if (line.startsWith("/")) {
        const slash = handleSlashCommand(line, { info, session, baseUrl: options.baseUrl });
        const action = applySlash(slash);
        if (action === "quit") break;
        if (action === "clear") {
          session = null;
          printMeta("new session");
        }
        continue;
      }

      printUserLine(line);
      try {
        const { session: next, response } = await client.sendTurn(line, session, {
          wait: false,
          onEvent: (ev) => {
            if (ev.type === "turn:step" && ev.toolsUsed?.length) {
              printStep(ev.toolsUsed);
            }
            if (ev.type === "turn:error") {
              printError(ev.message ?? "turn failed");
            }
          },
        });
        session = next;
        if (response.error) {
          printError(response.message ?? response.error);
        } else if (response.text) {
          printAssistant(response.text);
        }
        if (response.steps !== undefined) {
          printMeta(`${response.steps} step(s)`);
        }
      } catch (err) {
        printError(String(err));
      }
      console.log("");
    }
  } finally {
    rl.close();
  }
}
