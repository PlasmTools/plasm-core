import {
  streamText,
  stepCountIs,
  type LanguageModel,
  type LanguageModelUsage,
  type ModelMessage,
  type TelemetryOptions,
  type ToolSet,
  type FinishReason,
  type StepResult,
} from "ai";

import {
  applyArtifactLedger,
  gateUnreadArtifactTerminals,
} from "../tools/artifact-contract.js";
import { successfulEvalTerminalInStep } from "../tools/format.js";
import { transientProviderFailure } from "./provider-failure.js";
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
  finishReason?: FinishReason;
  rawFinishReason?: string;
  usage?: LanguageModelUsage;
  /** Accumulated turn messages after this step's response delta. */
  messages?: ModelMessage[];
}

export interface EveToolLoopModelOptions {
  temperature?: number;
  maxOutputTokens?: number;
  topP?: number;
  topK?: number;
}

export type EveStepContext = { stepIndex: number };

export type EveToolsForStep = ToolSet | ((ctx: EveStepContext) => ToolSet);

/**
 * Optional per-step view of history for the next model call.
 * Must not mutate the persisted conversation. Flag-off loops omit this.
 */
export type EvePrepareMessages = (
  messages: readonly ModelMessage[],
  ctx: EveStepContext,
) => ModelMessage[];

export function messagesForStep(
  messages: readonly ModelMessage[],
  prepare: EvePrepareMessages | undefined,
  ctx: EveStepContext,
): ModelMessage[] {
  if (!prepare) return [...messages];
  return prepare(messages, ctx);
}

export interface EveToolLoopOptions {
  model: LanguageModel;
  system: string;
  tools: EveToolsForStep;
  messages: ModelMessage[];
  /**
   * Ephemeral per-step message composition (e.g. model-authored ledger state).
   * Applied at the start of each iteration; not written back into history.
   */
  prepareMessages?: EvePrepareMessages;
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
  /**
   * Optional wall-clock ceiling for one model generation. An aborted generation
   * without a started tool execution uses the normal provider-recovery path.
   * If execution has started, the loop stops instead of risking replay of a
   * side effect whose response was lost with the provider stream.
   */
  generationTimeoutMs?: number;
  /**
   * Optional cap on consecutive provider-generation failures. A successful
   * generation resets it; this is not an extra step budget.
   */
  maxConsecutiveProviderFailures?: number;
  /**
   * Optional tool choice for the first model step.
   * Later steps stay auto unless a reviewed `run_ref` or required
   * run snapshot is still unread, or initial discovery is still pending.
   */
  toolChoice?:
    | "auto"
    | "required"
    | "none"
    | { type: "tool"; toolName: string };
  /**
   * Eval lifecycle gate: while true, the loop requires `plasm_context` and
   * refuses prose-only exit until a valid discovery response exists (or a
   * session is already open via `discoveryCompleted`). Successful insufficient
   * responses count; malformed args / validation failure / non-execution do
   * not. After a valid response, prose clarification is allowed; terminals
   * still require an open workflow.
   */
  requireInitialDiscovery?: boolean;
  /**
   * Whether lifecycle gates may force a provider tool choice (initial
   * discovery or continuation with pending run refs/artifacts). Defaults true.
   * Set false for reasoning endpoints that reject forced tool choice; the
   * corresponding lifecycle gates still hold.
   */
  forceToolChoice?: boolean;
  /**
   * True once initial `plasm_context` has opened a workflow session.
   * Used with `requireInitialDiscovery` so a pre-opened session skips the force.
   */
  discoveryCompleted?: () => boolean;
}

/** Forced first tool while initial discovery has no valid response yet. */
export const INITIAL_DISCOVERY_TOOL_NAME = "plasm_context";

/**
 * Loop-exit telemetry. `unterminated` is the existing host grade vocabulary
 * (`EvalTerminalGrade.kind`) for a model stop with no validated terminal.
 * Do not reuse `budget_exhausted` for mid-budget prose exits.
 */
export type EveToolLoopStopReason =
  | "completed"
  | "budget_exhausted"
  | "unterminated"
  | "error"
  /** The configured consecutive provider-failure allowance was exhausted. */
  | "provider_exhausted";

export interface EveToolLoopResult {
  text: string;
  steps: StepResult<ToolSet>[];
  usage: LanguageModelUsage;
  messages: ModelMessage[];
  stopReason: EveToolLoopStopReason;
  /** Per-model-step finish reasons; raw provider `error` overrides mapped `other`. */
  stepFinishReasons: FinishReason[];
  /**
   * Per-generation output ceiling passed to the model (reasoning + text + tool
   * JSON share this). Undefined when the caller omitted `modelOptions.maxOutputTokens`.
   */
  maxOutputTokens?: number;
  /** Count of steps whose finishReason was `length` (generation truncated). */
  lengthTruncationCount: number;
  /** Generations aborted by the configured wall-clock deadline. */
  generationTimeoutCount: number;
}

/**
 * Surfaced to the model after InvalidToolInputError / truncated tool JSON.
 * Host never invents truncated fields; the model must re-emit valid args.
 * Budget: the failed generation already consumed one `maxSteps` unit; each
 * repair attempt is another ordinary step — not a free extra.
 */
export const INVALID_TOOL_INPUT_REPAIR_DIAGNOSTIC =
  "Host: tool arguments failed JSON or schema validation. Re-emit the same tool with complete, valid arguments. The host does not invent or fill truncated fields. This failed call counted as one step toward the step budget.";

/**
 * Surfaced when `finishReason === length` mid-budget with **no** successful
 * tool results this step — truncated before a complete tool call or terminal.
 * Continuation consumes another ordinary `maxSteps` unit — not a free extra,
 * not a grade path, and not host-invented `complete_task` / `submit_answer`.
 */
export const GENERATION_TRUNCATED_CONTINUE_DIAGNOSTIC =
  "Host: generation truncated (finishReason=length) — output token budget exhausted before a complete tool call or terminal. Continue the same task; re-emit the next tool or terminal with complete arguments. Raising PLASM_EVAL_MAX_STEPS alone does not enlarge per-generation output. This truncated generation counted as one step toward the step budget.";

/**
 * Surfaced when `finishReason === length` after one or more tools already
 * executed successfully this step. Successful results remain in the transcript;
 * the model must continue from them — not treat the step as "no tool ran."
 */
export const GENERATION_TRUNCATED_AFTER_TOOLS_DIAGNOSTIC =
  "Host: generation truncated (finishReason=length) after successful tool execution — successful tool results above are preserved. Continue the same task from those results; do not assume the tools failed or never ran. Re-emit the next tool or terminal with complete arguments. Raising PLASM_EVAL_MAX_STEPS alone does not enlarge per-generation output. This truncated generation counted as one step toward the step budget.";

export const GENERATION_TIMEOUT_CONTINUE_DIAGNOSTIC =
  "Host: provider generation exceeded its wall-clock deadline. Prior successful tool results are preserved; incomplete tool arguments from this generation were not executed. Continue the same task from the observed results with a complete next call. This timed-out generation counted toward the step budget.";

/** Select length-continue liturgy: empty/truncated vs post-success truncation. */
export function generationTruncatedContinueDiagnostic(
  hadSuccessfulToolResults: boolean,
): string {
  return hadSuccessfulToolResults
    ? GENERATION_TRUNCATED_AFTER_TOOLS_DIAGNOSTIC
    : GENERATION_TRUNCATED_CONTINUE_DIAGNOSTIC;
}

const INVALID_TOOL_INPUT_MARKERS = [
  "AI_InvalidToolInputError",
  "InvalidToolInputError",
] as const;

function textLooksLikeInvalidToolInput(text: string): boolean {
  return INVALID_TOOL_INPUT_MARKERS.some((marker) => text.includes(marker));
}

function outputLooksLikeToolError(output: unknown): boolean {
  if (!output || typeof output !== "object") return false;
  const out = output as { type?: unknown };
  return out.type === "error-text" || out.type === "error-json";
}

/**
 * True when this step produced an invalid/truncated tool-argument failure
 * (AI SDK InvalidToolInputError path), already returned as a tool error to
 * the model. Ordinary execute failures are not included.
 */
export function stepHasInvalidToolInput(
  messages: readonly ModelMessage[],
  toolResults?: ReadonlyArray<Record<string, unknown>>,
): boolean {
  for (const result of toolResults ?? []) {
    if (result.invalid === true) return true;
    const err = result.error;
    if (typeof err === "string" && textLooksLikeInvalidToolInput(err))
      return true;
    if (err instanceof Error && textLooksLikeInvalidToolInput(err.message))
      return true;
    const output = result.output;
    if (output && typeof output === "object") {
      const out = output as { type?: unknown; value?: unknown };
      if (
        (out.type === "error-text" || out.type === "error-json") &&
        typeof out.value === "string" &&
        textLooksLikeInvalidToolInput(out.value)
      ) {
        return true;
      }
    }
  }
  for (const message of messages) {
    if (message.role !== "tool") continue;
    const content = message.content;
    if (!Array.isArray(content)) continue;
    for (const part of content) {
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      if (rec.invalid === true) return true;
      const output = rec.output;
      if (output && typeof output === "object") {
        const out = output as { type?: unknown; value?: unknown };
        if (
          (out.type === "error-text" || out.type === "error-json") &&
          typeof out.value === "string" &&
          textLooksLikeInvalidToolInput(out.value)
        ) {
          return true;
        }
      }
      if (
        typeof rec.error === "string" &&
        textLooksLikeInvalidToolInput(rec.error)
      ) {
        return true;
      }
    }
  }
  return false;
}

/**
 * True when this step produced at least one successful tool execution result.
 * Used to distinguish length-before-tool from length-after-tool continuation.
 */
export function stepHasSuccessfulToolResult(
  messages: readonly ModelMessage[],
  toolResults?: ReadonlyArray<Record<string, unknown>>,
): boolean {
  for (const result of toolResults ?? []) {
    if (result.type === "tool-error" || result.invalid === true) continue;
    if (result.error !== undefined && result.error !== null) continue;
    if (outputLooksLikeToolError(result.output)) continue;
    if (result.type === "tool-result") return true;
    // SDK shapes sometimes omit `type` but still carry a successful output.
    if (result.output !== undefined) return true;
  }
  for (const message of messages) {
    if (message.role !== "tool") continue;
    const content = message.content;
    if (!Array.isArray(content)) continue;
    for (const part of content) {
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      if (rec.type === "tool-error" || rec.invalid === true) continue;
      if (rec.error !== undefined && rec.error !== null) continue;
      if (outputLooksLikeToolError(rec.output)) continue;
      if (rec.type === "tool-result") return true;
    }
  }
  return false;
}

/**
 * Satisfaction predicate for the initial-discovery force:
 * a successful `plasm_context` tool-result (including insufficient domain
 * outcomes). Naming the tool, schema/JSON validation failure, or any
 * non-execution path must not satisfy the gate.
 */
export function stepHasValidDiscoveryResponse(
  messages: readonly ModelMessage[],
  toolResults?: ReadonlyArray<Record<string, unknown>>,
): boolean {
  for (const result of toolResults ?? []) {
    if (result.toolName !== INITIAL_DISCOVERY_TOOL_NAME) continue;
    if (result.type === "tool-error" || result.invalid === true) continue;
    if (result.error !== undefined && result.error !== null) continue;
    if (outputLooksLikeToolError(result.output)) continue;
    if (result.type === "tool-result") return true;
    if (result.output !== undefined) return true;
  }
  for (const message of messages) {
    if (message.role !== "tool") continue;
    const content = message.content;
    if (!Array.isArray(content)) continue;
    for (const part of content) {
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      if (rec.toolName !== INITIAL_DISCOVERY_TOOL_NAME) continue;
      if (rec.type === "tool-error" || rec.invalid === true) continue;
      if (rec.error !== undefined && rec.error !== null) continue;
      if (outputLooksLikeToolError(rec.output)) continue;
      if (rec.type === "tool-result") return true;
    }
  }
  return false;
}

/** Non-terminal finish: error, mid-budget model stop, or true max-steps. */
export function classifyNonTerminalStop(
  finishReason: string,
  stepsUsed: number,
  maxSteps: number,
): Exclude<EveToolLoopStopReason, "completed"> {
  if (finishReason === "error") return "error";
  if (stepsUsed < maxSteps) return "unterminated";
  return "budget_exhausted";
}

/**
 * Mid-budget `finishReason=length` with no validated terminal must continue
 * within the remaining step budget. Does not convert length into success.
 * Invalid/truncated tool JSON uses the repair path instead (caller checks that first).
 */
export function shouldContinueAfterGenerationLength(options: {
  finishReason: string;
  stepsUsed: number;
  maxSteps: number;
  hasValidatedTerminal: boolean;
}): boolean {
  if (options.finishReason !== "length") return false;
  if (options.hasValidatedTerminal) return false;
  return options.stepsUsed < options.maxSteps;
}

const RUN_REF_PASS = /pass `run_ref`:\s*`([^`]+)`/g;
const RUN_REF_PAGES = /plasm_run with run_ref:\s*"([^"]+)"/g;

/** Reviewed write / page handles still waiting for `plasm_run`. */
export function runRefsInText(text: string): string[] {
  const found = new Set<string>();
  for (const re of [RUN_REF_PASS, RUN_REF_PAGES]) {
    re.lastIndex = 0;
    let match: RegExpExecArray | null;
    while ((match = re.exec(text)) !== null) {
      if (match[1]) found.add(match[1]);
    }
  }
  return [...found];
}

function partText(part: unknown): string {
  if (typeof part === "string") return part;
  if (!part || typeof part !== "object") return "";
  const rec = part as Record<string, unknown>;
  if (typeof rec.text === "string") return rec.text;
  if (typeof rec.result === "string") return rec.result;
  if (typeof rec.output === "string") return rec.output;
  const output = rec.output;
  if (
    output &&
    typeof output === "object" &&
    typeof (output as { value?: unknown }).value === "string"
  ) {
    return (output as { value: string }).value;
  }
  return "";
}

function toolCallIdOf(rec: Record<string, unknown>): string {
  if (typeof rec.toolCallId === "string") return rec.toolCallId;
  if (typeof rec.toolCallID === "string") return rec.toolCallID;
  return "";
}

function runRefFromToolInput(input: unknown): string | null {
  if (typeof input === "string") {
    try {
      return runRefFromToolInput(JSON.parse(input));
    } catch {
      const match = /"run_ref"\s*:\s*"([^"]+)"/.exec(input);
      return match?.[1] ?? null;
    }
  }
  if (input && typeof input === "object") {
    const ref = (input as { run_ref?: unknown }).run_ref;
    if (typeof ref === "string" && ref.trim()) return ref.trim();
  }
  return null;
}

function toolResultText(
  content: ModelMessage["content"],
): { toolName: string; text: string; toolCallId: string; failed: boolean }[] {
  if (typeof content === "string")
    return [{ toolName: "", text: content, toolCallId: "", failed: false }];
  if (!Array.isArray(content)) return [];
  return content.map((part) => {
    const rec =
      part && typeof part === "object" ? (part as Record<string, unknown>) : {};
    const toolName = typeof rec.toolName === "string" ? rec.toolName : "";
    return {
      toolName,
      text: partText(part),
      toolCallId: toolCallIdOf(rec),
      failed:
        rec.type === "tool-error" ||
        rec.invalid === true ||
        (rec.error !== undefined && rec.error !== null) ||
        outputLooksLikeToolError(rec.output),
    };
  });
}

function consumedRunRefs(messages: ModelMessage[]): Map<string, string> {
  const consumed = new Map<string, string>();
  for (const message of messages) {
    if (message.role !== "assistant") continue;
    const content = message.content;
    if (!Array.isArray(content)) continue;
    for (const part of content) {
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      const name = typeof rec.toolName === "string" ? rec.toolName : "";
      if (name !== "plasm_run") continue;
      const ref = runRefFromToolInput(rec.input ?? rec.args);
      if (!ref) continue;
      const id = toolCallIdOf(rec) || `__anon_${consumed.size}`;
      consumed.set(id, ref);
    }
  }
  return consumed;
}

export function applyRunRefLedger(
  outstanding: Set<string>,
  messages: ModelMessage[],
): void {
  const consumed = consumedRunRefs(messages);
  for (const message of messages) {
    if (message.role !== "tool") continue;
    for (const {
      toolName,
      text,
      toolCallId,
      failed: taggedError,
    } of toolResultText(message.content)) {
      if (toolName === "plasm_run") {
        const failed =
          taggedError ||
          /\*\*plasm_run\*\* error/i.test(text) ||
          /pending transport/i.test(text);
        if (!failed) {
          const handled =
            (toolCallId && consumed.get(toolCallId)) ||
            (consumed.size === 1 ? [...consumed.values()][0] : undefined);
          if (handled) outstanding.delete(handled);
          for (const ref of runRefsInText(text)) outstanding.add(ref);
        }
        continue;
      }
      if (toolName === "plasm" || toolName === "") {
        for (const ref of runRefsInText(text)) outstanding.add(ref);
      }
    }
  }
}

function addUsage(
  a: LanguageModelUsage,
  b: LanguageModelUsage,
): LanguageModelUsage {
  const sum = (x: number | undefined, y: number | undefined) =>
    x === undefined || y === undefined ? undefined : x + y;
  return {
    inputTokens: sum(a.inputTokens, b.inputTokens),
    outputTokens: sum(a.outputTokens, b.outputTokens),
    totalTokens: sum(a.totalTokens, b.totalTokens),
    inputTokenDetails: {
      noCacheTokens: sum(
        a.inputTokenDetails.noCacheTokens,
        b.inputTokenDetails.noCacheTokens,
      ),
      cacheReadTokens: sum(
        a.inputTokenDetails.cacheReadTokens,
        b.inputTokenDetails.cacheReadTokens,
      ),
      cacheWriteTokens: sum(
        a.inputTokenDetails.cacheWriteTokens,
        b.inputTokenDetails.cacheWriteTokens,
      ),
    },
    outputTokenDetails: {
      textTokens: sum(
        a.outputTokenDetails.textTokens,
        b.outputTokenDetails.textTokens,
      ),
      reasoningTokens: sum(
        a.outputTokenDetails.reasoningTokens,
        b.outputTokenDetails.reasoningTokens,
      ),
    },
  };
}

/**
 * Eve-compatible tool loop: one `ai.eve.turn` parent span per step, `streamText`
 * child spans via AI SDK OTEL (`OpenTelemetry` + runtime context).
 */
export async function runEveToolLoop(
  options: EveToolLoopOptions,
): Promise<EveToolLoopResult> {
  if (!Number.isSafeInteger(options.maxSteps) || options.maxSteps < 1) {
    throw new Error("maxSteps must be a positive integer");
  }
  if (
    options.generationTimeoutMs !== undefined &&
    (!Number.isSafeInteger(options.generationTimeoutMs) ||
      options.generationTimeoutMs < 1)
  ) {
    throw new Error("generationTimeoutMs must be a positive integer when set");
  }
  if (
    options.maxConsecutiveProviderFailures !== undefined &&
    (!Number.isSafeInteger(options.maxConsecutiveProviderFailures) ||
      options.maxConsecutiveProviderFailures < 1)
  ) {
    throw new Error(
      "maxConsecutiveProviderFailures must be a positive integer when set",
    );
  }
  ensureOtelIntegration();

  const sessionId = options.sessionId ?? createEveSessionId();
  const turnSequence = options.turnSequence ?? 0;
  const turnId = options.turnId ?? createEveTurnId(turnSequence);
  const channelKind = options.channelKind ?? "unknown";

  let messages = [...options.messages];
  let stepIndex = 0;
  let finalText = "";
  let lastUsage: LanguageModelUsage | undefined;
  let stopReason: EveToolLoopResult["stopReason"] = "budget_exhausted";
  const aggregatedSteps: StepResult<ToolSet>[] = [];
  const stepFinishReasons: FinishReason[] = [];
  const outstandingRunRefs = new Set<string>();
  const outstandingArtifacts = new Set<string>();
  let forceTool = false;
  let consecutiveProviderFailures = 0;
  let generationTimeoutCount = 0;
  // Session already open → discovery satisfied. Else wait for a valid plasm_context response.
  let discoverySatisfied =
    !options.requireInitialDiscovery || options.discoveryCompleted?.() === true;

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

    const discoveryPending =
      options.requireInitialDiscovery === true && !discoverySatisfied;

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
        const stepTools = gateUnreadArtifactTerminals(
          typeof options.tools === "function"
            ? options.tools({ stepIndex })
            : options.tools,
          outstandingArtifacts,
        );
        // Discovery force outranks caller toolChoice and run_ref force until a
        // valid plasm_context response exists (clarification allowed after).
        // The lifecycle gate remains active when provider compatibility
        // requires auto tool choice.
        const stepToolChoice =
          discoveryPending && options.forceToolChoice !== false
          ? ({ type: "tool", toolName: INITIAL_DISCOVERY_TOOL_NAME } as const)
          : stepIndex === 0 && options.toolChoice !== undefined
            ? options.toolChoice
            : forceTool && options.forceToolChoice !== false
              ? ("required" as const)
              : undefined;
        let providerStreamErrorCount = 0;
        let providerStreamError: unknown;
        let generationTimedOut = false;
        let toolExecutionStarted = false;
        const abortController = new AbortController();
        const timeoutHandle =
          options.generationTimeoutMs === undefined
            ? undefined
            : setTimeout(() => {
                generationTimedOut = true;
                abortController.abort(
                  new Error(
                    `provider generation exceeded ${options.generationTimeoutMs}ms`,
                  ),
                );
              }, options.generationTimeoutMs);
        const streamResult = streamText({
          // Transport retries precede any tool execution. Failed generations
          // additionally consume the existing step/provider-failure budgets.
          maxRetries: 2,
          onToolExecutionStart: () => {
            toolExecutionStarted = true;
          },
          onError: ({ error }) => {
            providerStreamErrorCount += 1;
            providerStreamError ??= error;
          },
          model: options.model,
          system: options.system,
          tools: stepTools,
          messages: messagesForStep(messages, options.prepareMessages, {
            stepIndex,
          }),
          stopWhen: stepCountIs(1),
          abortSignal: abortController.signal,
          runtimeContext,
          experimental_telemetry: telemetry,
          ...(stepToolChoice !== undefined
            ? { toolChoice: stepToolChoice }
            : {}),
          ...(options.modelOptions?.temperature !== undefined
            ? { temperature: options.modelOptions.temperature }
            : {}),
          ...(options.modelOptions?.maxOutputTokens !== undefined
            ? { maxOutputTokens: options.modelOptions.maxOutputTokens }
            : {}),
          ...(options.modelOptions?.topP !== undefined
            ? { topP: options.modelOptions.topP }
            : {}),
          ...(options.modelOptions?.topK !== undefined
            ? { topK: options.modelOptions.topK }
            : {}),
        });
        let text: string;
        let finishReason: FinishReason;
        let rawFinishReason: string | undefined;
        let steps: StepResult<ToolSet>[];
        let usage: LanguageModelUsage;
        let response: { messages: ModelMessage[] };
        try {
          [text, finishReason, rawFinishReason, steps, usage, response] =
            await Promise.all([
              streamResult.text,
              streamResult.finishReason,
              streamResult.rawFinishReason,
              streamResult.steps,
              streamResult.usage,
              streamResult.response,
            ]);
        } catch (error: unknown) {
          const failure = providerStreamError ?? error;
          const transient = transientProviderFailure(failure);
          if (!generationTimedOut && (toolExecutionStarted || !transient)) {
            // Keep payment/auth errors and uncertain post-execution failures
            // fatal. An empty SDK response cannot reconstruct executed effects.
            throw failure;
          }
          providerStreamErrorCount = Math.max(1, providerStreamErrorCount);
          text = "";
          finishReason = "error";
          rawFinishReason = generationTimedOut ? "generation_timeout" : transient;
          steps = [];
          usage = {
            inputTokens: 0,
            outputTokens: 0,
            totalTokens: 0,
            inputTokenDetails: {
              noCacheTokens: 0,
              cacheReadTokens: 0,
              cacheWriteTokens: 0,
            },
            outputTokenDetails: { textTokens: 0, reasoningTokens: 0 },
          };
          response = { messages: [] };
        } finally {
          if (timeoutHandle !== undefined) clearTimeout(timeoutHandle);
        }

        // OpenAI-compatible providers can preserve raw `error` while mapping
        // it to unified `other` and emitting no SDK error event.
        const providerFailed =
          providerStreamErrorCount > 0 ||
          generationTimedOut ||
          finishReason === "error" ||
          rawFinishReason === "error";
        const effectiveFinishReason: FinishReason = providerFailed
          ? "error"
          : finishReason;
        return {
          text,
          finishReason: effectiveFinishReason,
          rawFinishReason,
          providerFailed,
          generationTimedOut,
          timedOutAfterToolExecution: generationTimedOut && toolExecutionStarted,
          steps,
          usage,
          response,
        };
      },
    );

    const lastStep = stepResult.steps.at(-1);
    const stepCalls = lastStep?.toolCalls ?? [];
    const stepToolResults = lastStep?.toolResults ?? [];

    finalText = stepResult.text;
    if (stepResult.generationTimedOut) generationTimeoutCount += 1;
    lastUsage = lastUsage
      ? addUsage(lastUsage, stepResult.usage)
      : stepResult.usage;
    aggregatedSteps.push(...stepResult.steps);
    stepFinishReasons.push(stepResult.finishReason);
    // The SDK response-message projection replaces invalid non-object inputs
    // with {}. Keep the emitted input from its tool-call record so persisted
    // history and the next provider request describe the actual failed call.
    const invalidInputs = new Map(
      stepCalls.flatMap((call) =>
        "invalid" in call && call.invalid === true
          ? [[call.toolCallId, call.input] as const]
          : [],
      ),
    );
    const delta = stepResult.response.messages.map((message) => {
      if (message.role !== "assistant" || !Array.isArray(message.content))
        return message;
      return {
        ...message,
        content: message.content.map((part) =>
          part.type === "tool-call" && invalidInputs.has(part.toolCallId)
            ? { ...part, input: invalidInputs.get(part.toolCallId) }
            : part,
        ),
      };
    });
    messages = [...messages, ...delta];

    // Valid plasm_context response (or pre-opened session) — not a mere tool name.
    if (
      stepHasValidDiscoveryResponse(delta, stepToolResults) ||
      options.discoveryCompleted?.() === true
    ) {
      discoverySatisfied = true;
    }

    await options.onStepFinish?.({
      toolCalls: stepCalls.length > 0 ? stepCalls : undefined,
      text: stepResult.text,
      finishReason: stepResult.finishReason,
      rawFinishReason: stepResult.rawFinishReason,
      usage: stepResult.usage,
      messages,
    });
    applyRunRefLedger(outstandingRunRefs, delta);
    applyArtifactLedger(outstandingArtifacts, delta);
    forceTool = outstandingRunRefs.size > 0 || outstandingArtifacts.size > 0;

    stepIndex += 1;
    consecutiveProviderFailures = stepResult.providerFailed
      ? consecutiveProviderFailures + 1
      : 0;
    const invalidToolInput = stepHasInvalidToolInput(delta, stepToolResults);
    // Invalid/truncated tool JSON is a recoverable observation, not loop exit.
    // Append a clear repair diagnostic; do not host-fill the truncated payload.
    if (
      !stepResult.providerFailed &&
      invalidToolInput &&
      stepIndex < options.maxSteps
    ) {
      messages = [
        ...messages,
        { role: "user", content: INVALID_TOOL_INPUT_REPAIR_DIAGNOSTIC },
      ];
    }
    const terminal = successfulEvalTerminalInStep({
      toolCalls: stepCalls,
      toolResults: stepToolResults,
      messages: delta,
    });
    const hasValidatedTerminal = Boolean(
      terminal && outstandingArtifacts.size === 0,
    );
    if (hasValidatedTerminal) {
      stopReason = "completed";
      break;
    }
    const providerFailureBudgetExhausted =
      stepResult.timedOutAfterToolExecution ||
      (options.maxConsecutiveProviderFailures !== undefined &&
        consecutiveProviderFailures >= options.maxConsecutiveProviderFailures);
    if (stepResult.providerFailed && providerFailureBudgetExhausted) {
      stopReason = "provider_exhausted";
      break;
    }
    if (stepResult.providerFailed && stepIndex < options.maxSteps) {
      messages = [
        ...messages,
        {
          role: "user",
          content: stepResult.generationTimedOut
            ? GENERATION_TIMEOUT_CONTINUE_DIAGNOSTIC
            : "Host: provider stream failed during generation. Successful tool results above are preserved; do not repeat their effects. Incomplete tool arguments were not executed. Continue from the observed results with a complete next call. This generation counted toward the step budget.",
        },
      ];
      continue;
    }
    if (stepResult.finishReason !== "tool-calls") {
      // Invalid/truncated tool JSON: repair path already appended; never execute bad args.
      if (invalidToolInput && stepIndex < options.maxSteps) {
        continue;
      }
      // Providers can finish reasoning with `stop` and emit no usable response.
      // This is not a task decision; preserve history and consume normal budget.
      if (
        stepResult.finishReason === "stop" &&
        stepResult.text.trim().length === 0 &&
        stepCalls.length === 0 &&
        stepIndex < options.maxSteps
      ) {
        messages = [
          ...messages,
          {
            role: "user",
            content:
              "Host: the previous generation ended without text or a tool call. Continue the same task from the observed results with a usable response. Prior successful tool results are preserved; do not repeat their effects. No task completion was recorded. This generation counted toward the step budget.",
          },
        ];
        continue;
      }
      // Output-token ceiling hit with unfinished work: continue, do not grade as done.
      // Distinguish empty/truncated length from length after successful tool results.
      if (
        shouldContinueAfterGenerationLength({
          finishReason: stepResult.finishReason,
          stepsUsed: stepIndex,
          maxSteps: options.maxSteps,
          hasValidatedTerminal,
        })
      ) {
        const hadSuccessfulTools = stepHasSuccessfulToolResult(
          delta,
          stepToolResults,
        );
        messages = [
          ...messages,
          {
            role: "user",
            content: generationTruncatedContinueDiagnostic(hadSuccessfulTools),
          },
        ];
        continue;
      }
      const discoveryStillPending =
        options.requireInitialDiscovery === true && !discoverySatisfied;
      if (
        (outstandingRunRefs.size > 0 ||
          outstandingArtifacts.size > 0 ||
          discoveryStillPending) &&
        stepIndex < options.maxSteps
      ) {
        continue;
      }
      stopReason = classifyNonTerminalStop(
        stepResult.finishReason,
        stepIndex,
        options.maxSteps,
      );
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
    messages,
    stopReason,
    stepFinishReasons,
    maxOutputTokens: options.modelOptions?.maxOutputTokens,
    lengthTruncationCount: stepFinishReasons.filter((r) => r === "length")
      .length,
    generationTimeoutCount,
  };
}
