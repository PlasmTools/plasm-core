import path from "node:path";
import { pathToFileURL } from "node:url";

import type { PlasmAgent } from "../runtime/plasm-agent.js";

export interface AuthoringContext {
  agentRoot: string;
  getAgent: () => Promise<PlasmAgent>;
  /** Dynamic import of a generated stub module by catalog `entry_id`. */
  importStub: (entryId: string) => Promise<unknown>;
}

export interface CreateAuthoringContextOptions {
  agentRoot: string;
  getAgent: () => Promise<PlasmAgent>;
  importCacheBust?: number;
}

export function createAuthoringContext(
  options: CreateAuthoringContextOptions,
): AuthoringContext {
  const agentRoot = path.resolve(options.agentRoot);
  const bust = options.importCacheBust ?? Date.now();

  return {
    agentRoot,
    getAgent: options.getAgent,
    importStub: async (entryId: string) => {
      const stubPath = path.join(agentRoot, ".plasm", "stubs", `${entryId}.ts`);
      const url = `${pathToFileURL(stubPath).href}?t=${bust}`;
      return import(url);
    },
  };
}
