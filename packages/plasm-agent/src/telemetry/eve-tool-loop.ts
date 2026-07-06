import {
  streamText,
  stepCountIs,
  type LanguageModel,
  type LanguageModelUsage,
  type ModelMessage,
  type TelemetryOptions,
  type ToolSet,
} from "ai";
import type { Context } from "@ai-sdk/provider-utils";

import { ensureOtelIntegration } from "../instrumentation.js";
import {
  buildEveRuntimeContext,
  createEveSessionId,
  createEveTurnId,
  enrichEveTelemetry,
  withEveTurnSpan,
  type EveChannelKind,
} from "./eve-agent-runs.js";

export interface AgentStepEvent {
  toolCalls?: Array<{ toolName: string }>;
  text?: string;
  finishReason?: string;
  usage?: LanguageModelUsage;
}

export interface EveToolLoopModelOptions {
  temperature?: number;
  maxOutputTokens?: number;
  topP?: number;
  topK?: number;
}

export interface EveToolLoopOptions {
  model: LanguageModel;
  system: string;
  tools: ToolSet;
  messages: ModelMessage[];
  maxSteps: number;
  agentName: string;
  channelKind?: EveChannelKind;
  /** Workflow session run id (`wrun_*`) for Agent Runs OTEL linkage. */
  sessionId?: string;
  turnId?: string;
  turnSequence?: number;
  telemetry?: TelemetryOptions;
  onStepStart?: () => void | Promise<void>;
  onStepFinish?: (step: AgentStepEvent) => void | Promise<void>;
  modelOptions?: EveToolLoopModelOptions;
}

export interface EveToolLoopResult {
  text: string;
  steps: unknown[];
  usage: LanguageModelUsage;
}

/**
 * Eve-compatible tool loop: one `ai.eve.turn` parent span per step, `streamText`
 * child spans via AI SDK OTEL (`OpenTelemetry` + runtime context).
 */
export async function runEveToolLoop(options: EveToolLoopOptions): Promise<EveToolLoopResult> {
  ensureOtelIntegration();

  const sessionId = options.sessionId ?? createEveSessionId();
  const turnSequence = options.turnSequence ?? 0;
  const turnId = options.turnId ?? createEveTurnId(turnSequence);
  const channelKind = options.channelKind ?? "unknown";

  let messages = options.messages;
  let stepIndex = 0;
  let finalText = "";
  let lastUsage: LanguageModelUsage | undefined;
  const aggregatedSteps: unknown[] = [];

  while (stepIndex < options.maxSteps) {
    await options.onStepStart?.();

    const runtimeContext = buildEveRuntimeContext({
      sessionId,
      turnId,
      sequence: turnSequence,
      stepIndex,
      channelKind,
    });
    const telemetry = enrichEveTelemetry(
      options.telemetry ?? { isEnabled: true, functionId: options.agentName },
      runtimeContext,
    );

    const stepResult = await withEveTurnSpan(
      {
        sessionId,
        turnId,
        sequence: turnSequence,
        stepIndex,
        channelKind,
        functionId: options.agentName,
      },
      async () => {
        const streamResult = streamText({
          model: options.model,
          system: options.system,
          tools: options.tools,
          messages,
          stopWhen: stepCountIs(1),
          runtimeContext: runtimeContext as Context,
          experimental_telemetry: telemetry,
          ...(options.modelOptions?.temperature !== undefined
            ? { temperature: options.modelOptions.temperature }
            : {}),
          ...(options.modelOptions?.maxOutputTokens !== undefined
            ? { maxOutputTokens: options.modelOptions.maxOutputTokens }
            : {}),
          ...(options.modelOptions?.topP !== undefined ? { topP: options.modelOptions.topP } : {}),
          ...(options.modelOptions?.topK !== undefined ? { topK: options.modelOptions.topK } : {}),
        });

        const [text, finishReason, steps, usage, response] = await Promise.all([
          streamResult.text,
          streamResult.finishReason,
          streamResult.steps,
          streamResult.usage,
          streamResult.response,
        ]);

        return { text, finishReason, steps, usage, response };
      },
    );

    const stepCalls =
      stepResult.steps.length > 0
        ? ((stepResult.steps.at(-1) as { toolCalls?: Array<{ toolName: string }> } | undefined)
            ?.toolCalls ?? [])
        : [];

    await options.onStepFinish?.({
      toolCalls: stepCalls.length > 0 ? stepCalls : undefined,
      text: stepResult.text,
      finishReason: stepResult.finishReason,
      usage: stepResult.usage,
    });

    finalText = stepResult.text;
    lastUsage = stepResult.usage;
    aggregatedSteps.push(...stepResult.steps);
    messages = stepResult.response.messages as ModelMessage[];

    stepIndex += 1;
    if (stepResult.finishReason !== "tool-calls") {
      break;
    }
  }

  if (!lastUsage) {
    throw new Error("eve tool loop produced no model steps");
  }

  return {
    text: finalText,
    steps: aggregatedSteps,
    usage: lastUsage,
  };
}
