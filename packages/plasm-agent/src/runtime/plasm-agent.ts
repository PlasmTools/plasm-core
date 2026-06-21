import { readFile } from "node:fs/promises";
import path from "node:path";

import { generateText, stepCountIs, type LanguageModel, type ModelMessage, type ToolSet } from "ai";

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
import { createPlasmTools } from "../tools/plasm-tools.js";

export interface PlasmAgentConfig extends AgentRuntimeConfig {
  /** AI Gateway model slug, e.g. `anthropic/claude-sonnet-4.6`. */
  model: string | LanguageModel;
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

export interface AgentStepEvent {
  toolCalls?: Array<{ toolName: string }>;
  text?: string;
  finishReason?: string;
}

export interface AgentGenerateOptions {
  messages?: ModelMessage[];
  resetConversation?: boolean;
  onStepFinish?: (step: AgentStepEvent) => void | Promise<void>;
}

export interface AgentTurnResult {
  text: string;
  steps: unknown[];
  usage: Awaited<ReturnType<typeof generateText>>["usage"];
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
  private conversation: ModelMessage[] = [];

  constructor(config: PlasmAgentConfig) {
    this.runtime = new AgentRuntime(config);
    this.model = config.model;
    this.modelOptions = config.modelOptions;
    this.instructionsPath =
      config.instructionsPath ?? path.join(config.agentRoot, "instructions.md");
    this.maxSteps = config.maxSteps ?? 20;
    this.telemetryEnabled = config.telemetry ?? true;
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
    let base: string;
    try {
      base = (await readFile(this.instructionsPath, "utf8")).trim();
    } catch {
      base = [
        "You are a catalog-native Plasm agent.",
        "Use discover_capabilities → plasm_context → plasm → plasm_run.",
        "Keep one stable intent per user goal.",
      ].join("\n");
    }

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
    });
    const tools = {
      ...plasmTools,
      ...harnessTools,
    } as ToolSet;

    const telemetry = this.telemetryEnabled
      ? createAgentTelemetry({ serviceName: "plasm-agent" })
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

    const generation = {
      model,
      system,
      tools,
      stopWhen: stepCountIs(this.maxSteps),
      experimental_telemetry: telemetry,
      onStepFinish: async (step: AgentStepEvent) => {
        await options.onStepFinish?.(step);
        if (!this.hookRunner || !this.getAuthoringContext) return;
        const toolsUsed = (step.toolCalls ?? []).map((call) => call.toolName);
        await this.hookRunner.emit("agent:step", this.getAuthoringContext(), { toolsUsed });
      },
      ...(this.modelOptions?.temperature !== undefined
        ? { temperature: this.modelOptions.temperature }
        : {}),
      ...(this.modelOptions?.maxOutputTokens !== undefined
        ? { maxOutputTokens: this.modelOptions.maxOutputTokens }
        : {}),
      ...(this.modelOptions?.topP !== undefined ? { topP: this.modelOptions.topP } : {}),
      ...(this.modelOptions?.topK !== undefined ? { topK: this.modelOptions.topK } : {}),
    };

    const result = await generateText({
      ...generation,
      messages,
    });

    if (!externalMessages) {
      this.conversation.push({ role: "assistant", content: result.text });
    }
    return {
      text: result.text,
      steps: result.steps,
      usage: result.usage,
    };
  }
}
