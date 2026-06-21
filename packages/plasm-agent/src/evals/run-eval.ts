import { readdir } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

import type { PlasmAgent } from "../runtime/plasm-agent.js";
import type { EvalDefinition, EvalToolName } from "./define-eval.js";
import { isEvalDefinition } from "./define-eval.js";

export interface EvalRunResult {
  name: string;
  ok: boolean;
  recordedTools: EvalToolName[];
  responseText?: string;
  stepCount: number;
  error?: string;
}

function collectToolNames(steps: unknown[]): EvalToolName[] {
  const names = new Set<EvalToolName>();
  for (const step of steps) {
    if (!step || typeof step !== "object") continue;
    const toolCalls = (step as { toolCalls?: Array<{ toolName?: string }> }).toolCalls ?? [];
    for (const call of toolCalls) {
      const name = call.toolName;
      if (name) names.add(name as EvalToolName);
    }
  }
  return [...names];
}

async function importEvalModule(filePath: string): Promise<EvalDefinition> {
  const mod = await import(pathToFileURL(filePath).href);
  const exported = mod.default ?? mod;
  if (!isEvalDefinition(exported)) {
    throw new Error(`default export must be defineEval() result`);
  }
  return exported;
}

export async function discoverEvalFiles(evalsDir: string): Promise<string[]> {
  const entries = await readdir(evalsDir, { withFileTypes: true });
  return entries
    .filter((entry) => entry.isFile() && entry.name.endsWith(".eval.ts"))
    .map((entry) => path.join(evalsDir, entry.name))
    .sort((a, b) => a.localeCompare(b));
}

/** Run one live LLM eval via agent.generate(). */
export async function runEval(
  agent: PlasmAgent,
  definition: EvalDefinition,
): Promise<EvalRunResult> {
  let recordedTools: EvalToolName[] = [];
  let responseText: string | undefined;
  let stepCount = 0;

  try {
    const result = await agent.generate(definition.goal, { resetConversation: true });
    responseText = result.text;
    stepCount = result.steps.length;
    recordedTools = collectToolNames(result.steps);

    if (definition.assert.minSteps !== undefined && stepCount < definition.assert.minSteps) {
      throw new Error(`expected at least ${definition.assert.minSteps} steps, got ${stepCount}`);
    }

    if (definition.assert.toolsUsedAny?.length) {
      for (const tool of definition.assert.toolsUsedAny) {
        if (!recordedTools.includes(tool)) {
          throw new Error(`expected tool ${tool} in live run, got [${recordedTools.join(", ")}]`);
        }
      }
    }

    if (definition.assert.responseIncludes) {
      const needle = definition.assert.responseIncludes;
      const haystack = responseText ?? "";
      const matches =
        typeof needle === "string" ? haystack.includes(needle) : needle.test(haystack);
      if (!matches) {
        throw new Error(`response missing expected fragment`);
      }
    }

    return {
      name: definition.name,
      ok: true,
      recordedTools,
      responseText,
      stepCount,
    };
  } catch (err) {
    return {
      name: definition.name,
      ok: false,
      recordedTools,
      responseText,
      stepCount,
      error: String(err),
    };
  }
}

export async function runEvalFile(
  agent: PlasmAgent,
  evalPath: string,
): Promise<EvalRunResult> {
  const definition = await importEvalModule(evalPath);
  return runEval(agent, definition);
}

export function requireLiveEvalGateway(): void {
  const key =
    process.env.AI_GATEWAY_API_KEY?.trim() ||
    process.env.AI_API_GATEWAY_KEY?.trim() ||
    process.env.AI_GATEWAY_KEY?.trim();
  if (!key) {
    throw new Error(
      "Live evals require AI_GATEWAY_API_KEY (Vercel AI Gateway). Fixture-only eval chains are not supported.",
    );
  }
  process.env.AI_GATEWAY_API_KEY ??= key;
  process.env.PLASM_AGENT_MOCK_HTTP = "1";
}

export async function runAllEvals(
  agent: PlasmAgent,
  evalsDir: string,
): Promise<EvalRunResult[]> {
  const files = await discoverEvalFiles(evalsDir);
  const results: EvalRunResult[] = [];
  for (const file of files) {
    results.push(await runEvalFile(agent, file));
  }
  return results;
}
