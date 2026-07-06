import path from "node:path";
import { pathToFileURL } from "node:url";

import type { AgentDefinition } from "../define-agent.js";
import { frameworkPackageVersion } from "../package-version.js";
import { createPlasmApp } from "../server/plasm-handler.js";
import type { EveChannelKind } from "./eve-agent-runs.js";
import { createEveTurnId } from "./eve-agent-runs.js";
import {
  emitMessageCompleted,
  emitMessageReceived,
  emitSessionFailed,
  emitSessionStarted,
  emitSessionWaiting,
  emitStepCompleted,
  emitStepStarted,
  emitTurnCompleted,
  emitTurnFailed,
  emitTurnStarted,
  emitActionsRequested,
  type EveWorkflowWritable,
} from "./eve-message-stream.js";
import { buildTurnUsageAttributes } from "./eve-workflow-tags.js";
import { setEveWorkflowAttributes } from "./eve-workflow-runtime.js";

export interface EveWorkflowTurnOptions {
  agentRoot: string;
  /** Session workflow run id (`wrun_*`) — becomes `eve.session.id` on spans. */
  sessionRunId: string;
  channelKind?: EveChannelKind;
  turnSequence?: number;
  userMessage: string;
  writable: EveWorkflowWritable;
  rootSessionId?: string;
  agentName?: string;
  modelId?: string;
}

export interface EveWorkflowTurnResult {
  ok: boolean;
  skipped?: boolean;
  reason?: string;
  text?: string;
  error?: string;
  modelId?: string;
  usage?: {
    inputTokens?: number;
    outputTokens?: number;
    inputTokenDetails?: { cacheReadTokens?: number; cacheWriteTokens?: number };
  };
  toolCount?: number;
}

async function loadAgentDefinition(agentRoot: string): Promise<AgentDefinition> {
  const agentModulePath = path.join(agentRoot, "agent", "agent.ts");
  const mod = (await import(pathToFileURL(agentModulePath).href)) as {
    default?: AgentDefinition;
  };
  if (!mod.default) {
    throw new Error(`agent/agent.ts must default-export defineAgent(...)`);
  }
  return mod.default;
}

async function ensureAgentInstrumentation(agentRoot: string): Promise<void> {
  const instrumentationPath = path.join(agentRoot, "agent", "instrumentation.ts");
  try {
    const mod = (await import(pathToFileURL(instrumentationPath).href)) as {
      register?: () => void;
    };
    mod.register?.();
  } catch {
    // Optional — framework OTEL still registers via ensureOtelIntegration in tool loop.
  }
}

export async function runEveWorkflowTurn(
  options: EveWorkflowTurnOptions,
): Promise<EveWorkflowTurnResult> {
  const sequence = options.turnSequence ?? 0;
  const turnId = createEveTurnId(sequence);
  const channelKind = options.channelKind ?? "unknown";

  await emitSessionStarted(options.writable, {
    sessionId: options.sessionRunId,
    agentName: options.agentName,
    modelId: options.modelId,
    generatorVersion: frameworkPackageVersion(),
  });
  await emitTurnStarted(options.writable, { turnId, sequence });
  await emitMessageReceived(options.writable, {
    message: options.userMessage,
    turnId,
    sequence,
  });

  try {
    await ensureAgentInstrumentation(options.agentRoot);
    const definition = await loadAgentDefinition(options.agentRoot);
    const app = await createPlasmApp({
      agentRoot: options.agentRoot,
      definition,
      mode: "prod",
      sessions: false,
    });
    const agent = await app.getAgent();
    const resolvedModelId =
      options.modelId ??
      (typeof definition.model === "string" ? definition.model : undefined) ??
      "unknown";

    let stepIndex = 0;
    const turn = await agent.generate(options.userMessage, {
      resetConversation: false,
      channelKind,
      sessionId: options.sessionRunId,
      turnId,
      turnSequence: sequence,
      onStepStart: async () => {
        await emitStepStarted(options.writable, { turnId, stepIndex, sequence });
      },
      onStepFinish: async (step) => {
        const toolCalls = step.toolCalls ?? [];
        if (toolCalls.length > 0) {
          await emitActionsRequested(options.writable, {
            turnId,
            stepIndex,
            sequence,
            actions: toolCalls.map((call, index) => ({
              callId: `call_${stepIndex}_${index}`,
              toolName: call.toolName,
              kind: "tool-call" as const,
            })),
          });
        }
        await emitStepCompleted(options.writable, {
          turnId,
          stepIndex,
          sequence,
          finishReason: step.finishReason,
          usage: step.usage
            ? {
                inputTokens: step.usage.inputTokens,
                outputTokens: step.usage.outputTokens,
                cacheReadTokens: step.usage.inputTokenDetails?.cacheReadTokens,
                cacheWriteTokens: step.usage.inputTokenDetails?.cacheWriteTokens,
              }
            : undefined,
        });
        if (step.text?.trim()) {
          await emitMessageCompleted(options.writable, {
            turnId,
            stepIndex,
            sequence,
            message: step.text,
            finishReason: step.finishReason,
          });
        }
        stepIndex += 1;
      },
    });

    await setEveWorkflowAttributes(
      buildTurnUsageAttributes({
        modelId: resolvedModelId,
        usage: turn.usage,
        toolCount: turn.toolCount,
      }),
    );

    await emitTurnCompleted(options.writable, { turnId, sequence });
    await emitSessionWaiting(options.writable, { sessionId: options.sessionRunId });

    return {
      ok: true,
      text: turn.text,
      modelId: resolvedModelId,
      usage: turn.usage,
    };
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    await emitTurnFailed(options.writable, {
      turnId,
      sequence,
      code: "TURN_FAILED",
      message,
    });
    await emitSessionFailed(options.writable, {
      sessionId: options.sessionRunId,
      code: "SESSION_FAILED",
      message,
    });
    return { ok: false, error: message };
  }
}

export type { EveWorkflowWritable } from "./eve-message-stream.js";
