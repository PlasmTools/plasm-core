import { readFile } from "node:fs/promises";
import path from "node:path";

import { type LanguageModel, type ModelMessage, type ToolSet } from "ai";

import type {
  AgentBuildConfig,
  AgentCompactionConfig,
  AgentExperimentalConfig,
  AgentModelOptions,
} from "../define-agent.js";
import type { AuthoringContext } from "../authoring/context.js";
import type { HookRunner } from "../authoring/hook-runner.js";
import type { SkillDefinition } from "../authoring/define-skill.js";
import type { SubagentRegistry } from "../authoring/subagent-loader.js";
import { resolveGatewayModel } from "../gateway-model.js";
import { createAgentTelemetry } from "../instrumentation.js";
import { maybeCompactMessages } from "../runtime/compaction.js";
import { AgentRuntime, type AgentRuntimeConfig } from "../runtime/agent-runtime.js";
import { createHarnessTools, renderSkillIndex } from "../tools/harness-tools.js";
import { gateArtefactTransform } from "../tools/format.js";
import { createPlasmTools } from "../tools/plasm-tools.js";
import { buildDefaultSystemLiturgy } from "../prompts/index.js";
import { runEveToolLoop, type AgentStepEvent } from "../telemetry/eve-tool-loop.js";
import type { EveChannelKind } from "../telemetry/eve-agent-runs.js";

export type { AgentStepEvent };

export interface PlasmAgentConfig extends AgentRuntimeConfig {
  /** AI Gateway model slug, e.g. `anthropic/claude-sonnet-4.6`. */
  model: string | LanguageModel;
  /** Agent Runs / OTEL function id (defaults from agent root directory name). */
  agentName?: string;
  instructionsPath?: string;
  maxSteps?: number;
  telemetry?: boolean;
  compaction?: AgentCompactionConfig;
  modelOptions?: AgentModelOptions;
  build?: AgentBuildConfig;
  experimental?: AgentExperimentalConfig;
  loadedSkills?: SkillDefinition[];
  hookRunner?: HookRunner;
  subagentRegistry?: SubagentRegistry;
  getAuthoringContext?: () => AuthoringContext;
}

export interface AgentGenerateOptions {
  messages?: ModelMessage[];
  resetConversation?: boolean;
  onStepStart?: () => void | Promise<void>;
  onStepFinish?: (step: AgentStepEvent) => void | Promise<void>;
  /**
   * Optional tool adapter. Observers must preserve result and error semantics.
   */
  wrapTools?: (tools: ToolSet) => ToolSet | Promise<ToolSet>;
  /** Force tool use for this generate (eval: block prose-only refusals). */
  toolChoice?: "auto" | "required" | "none" | { type: "tool"; toolName: string };
  /** Override agent maxSteps for this generate only. */
  maxSteps?: number;
  /** Workflow session run id (`wrun_*`) — links OTEL spans to Agent Runs. */
  sessionId?: string;
  turnId?: string;
  turnSequence?: number;
  /** Eve Agent Runs channel kind (`schedule`, `http`, `channel:<name>`, …). */
  channelKind?: EveChannelKind;
}

export interface AgentTurnResult {
  text: string;
  steps: unknown[];
  usage: Awaited<ReturnType<typeof runEveToolLoop>>["usage"];
  /** Number of tools registered for this turn (not invocations). */
  toolsAvailable: number;
  /** Tool call invocations observed across steps. */
  toolCount: number;
  /** Ordered tool names invoked this turn. */
  toolInvocations: string[];
  messages: ModelMessage[];
  stopReason: Awaited<ReturnType<typeof runEveToolLoop>>["stopReason"];
}

export class PlasmAgent {
  readonly runtime: AgentRuntime;
  private readonly model: string | LanguageModel;
  private readonly instructionsPath: string;
  private readonly maxSteps: number;
  private readonly modelOptions?: AgentModelOptions;
  private readonly telemetryEnabled: boolean;
  private readonly loadedSkills: SkillDefinition[];
  private readonly skillsMode: false | "index" | "inline";
  private readonly compaction?: AgentCompactionConfig;
  private readonly hookRunner?: HookRunner;
  private readonly subagentRegistry?: SubagentRegistry;
  private readonly getAuthoringContext?: () => AuthoringContext;
  private readonly agentName: string;
  private conversation: ModelMessage[] = [];

  constructor(config: PlasmAgentConfig) {
    this.runtime = new AgentRuntime(config);
    this.model = config.model;
    this.modelOptions = config.modelOptions;
    this.instructionsPath =
      config.instructionsPath ?? path.join(config.agentRoot, "instructions.md");
    this.maxSteps = config.maxSteps ?? 20;
    this.telemetryEnabled = config.telemetry ?? true;
    this.agentName =
      config.agentName?.trim() ||
      process.env.PLASM_AGENT_NAME?.trim() ||
      path.basename(path.dirname(config.agentRoot));
    this.loadedSkills = config.loadedSkills ?? [];
    this.compaction = config.compaction;
    const skillsFlag = config.experimental?.skills;
    if (this.loadedSkills.length === 0 || skillsFlag === false) {
      this.skillsMode = false;
    } else if (skillsFlag === "inline") {
      this.skillsMode = "inline";
    } else {
      this.skillsMode = "index";
    }
    this.hookRunner = config.hookRunner;
    this.subagentRegistry = config.subagentRegistry;
    this.getAuthoringContext = config.getAuthoringContext;
  }

  async bootstrap(): Promise<void> {
    await this.runtime.bootstrap();
  }

  async loadInstructions(): Promise<string> {
    // Framework core: language law + resource rites (same bytes as MCP tool cards).
    const core = buildDefaultSystemLiturgy();
    let project = "";
    try {
      project = (await readFile(this.instructionsPath, "utf8")).trim();
    } catch {
      project = "";
    }
    // Placeholder / empty project files → core only.
    const isPlaceholder =
      !project ||
      /^#\s*Placeholder\b/i.test(project) ||
      /^#\s*Catalog-native Plasm agent\b/i.test(project);    const base = isPlaceholder
      ? core
      : `${core}\n\n# Project instructions\n\n${project}`;

    if (this.skillsMode === "inline") {
      const skillBlock = this.loadedSkills
        .map((skill) => `## Skill: ${skill.name}\n${skill.body.trim()}`)
        .join("\n\n");
      return `${base}\n\n# Skills\n\n${skillBlock}`;
    }

    if (this.skillsMode === "index") {
      return `${base}\n\n${renderSkillIndex(this.loadedSkills)}`;
    }

    return base;
  }

  async generate(
    prompt: string,
    options: AgentGenerateOptions = {},
  ): Promise<AgentTurnResult> {
    if (this.hookRunner && this.getAuthoringContext) {
      await this.hookRunner.emit("agent:start", this.getAuthoringContext(), { prompt });
    }

    const system = await this.loadInstructions();
    const plasmTools = createPlasmTools(this.runtime);
    const harnessTools = createHarnessTools({
      skills: this.skillsMode === "index" ? this.loadedSkills : undefined,
      subagents: this.subagentRegistry,
      artefactWorkspaceRoot: this.runtime.artefactWorkspaceRoot,
      includeArtefactTransform: true,
    });
    let tools = {
      ...plasmTools,
      ...harnessTools,
    } as ToolSet;
    if (options.wrapTools) {
      tools = await options.wrapTools(tools);
    }

    const telemetry = this.telemetryEnabled
      ? createAgentTelemetry({ serviceName: this.agentName })
      : { isEnabled: false };
    const model = resolveGatewayModel(this.model, this.modelOptions);

    const externalMessages = options.messages !== undefined;
    let messages: ModelMessage[];
    if (externalMessages) {
      messages = options.messages ?? [];
    } else if (options.resetConversation) {
      this.conversation = [{ role: "user", content: prompt }];
      messages = this.conversation;
    } else {
      this.conversation.push({ role: "user", content: prompt });
      messages = this.conversation;
    }

    messages = await maybeCompactMessages(messages, this.compaction, this.model);

    const toolInvocations: string[] = [];
    const onStepFinish = async (step: AgentStepEvent) => {
      for (const call of step.toolCalls ?? []) {
        toolInvocations.push(call.toolName);
      }
      await options.onStepFinish?.(step);
      if (!this.hookRunner || !this.getAuthoringContext) return;
      const toolsUsed = (step.toolCalls ?? []).map((call) => call.toolName);
      await this.hookRunner.emit("agent:step", this.getAuthoringContext(), { toolsUsed });
    };

    const result = await runEveToolLoop({
      model,
      system,
      tools: () => gateArtefactTransform(tools, this.runtime.hasMaterializedArtefact()),
      messages,
      maxSteps: options.maxSteps ?? this.maxSteps,
      agentName: this.agentName,
      channelKind: options.channelKind,
      sessionId: options.sessionId,
      turnId: options.turnId,
      turnSequence: options.turnSequence,
      telemetry,
      onStepStart: options.onStepStart,
      onStepFinish: async (step) => {
        await onStepFinish(step);
      },
      modelOptions: this.modelOptions,
      toolChoice: options.toolChoice,
    });

    if (!externalMessages) {
      this.conversation = result.messages;
    }
    return {
      text: result.text,
      steps: result.steps,
      usage: result.usage,
      toolsAvailable: Object.keys(tools).length,
      toolCount: toolInvocations.length,
      toolInvocations,
      messages: result.messages,
      stopReason: result.stopReason,
    };
  }
}
