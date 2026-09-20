import {
  streamText,
  stepCountIs,
  tool,
  type LanguageModel,
  type LanguageModelUsage,
  type ModelMessage,
  type ToolSet,
} from "ai";
import { z } from "zod";

import {
  COMPLETE_TASK_TOOL_NAME,
  SUBMIT_ANSWER_TOOL_NAME,
} from "./format.js";
import { emptyTaskLedger, type TaskLedgerRecord } from "./task-ledger.js";

/** Consequential gates: live write / page run, and finish proposals. */
export const TASK_LEDGER_REVIEW_GATED_TOOLS = [
  "plasm_run",
  COMPLETE_TASK_TOOL_NAME,
  SUBMIT_ANSWER_TOOL_NAME,
] as const;

export type TaskLedgerReviewGatedTool = (typeof TASK_LEDGER_REVIEW_GATED_TOOLS)[number];

const TASK_LEDGER_REVIEW_SUBMIT_TOOL_NAME = "submit_task_ledger_review";

export function isTaskLedgerReviewGatedTool(name: string): name is TaskLedgerReviewGatedTool {
  return (TASK_LEDGER_REVIEW_GATED_TOOLS as readonly string[]).includes(name);
}

const selectionConstraintProofSchema = z.object({
  constraint: z.string().min(1).max(500),
  evidence: z.string().min(1).max(1000),
  mutation_dataflow: z.string().min(1).max(1000),
});

/**
 * Host-binding decision plus review dimensions. The host does not recompute
 * domain truth from the ledger. It does require a positive, structurally
 * coherent proof judgment before a mutating `plasm_run` can pass.
 */
export const taskLedgerReviewVerdictSchema = z.object({
  decision: z.enum([
    "continue",
    "allow",
    "complete_without_value",
    "submit",
    "unresolved",
  ]),
  negative_evidence: z.enum(["sufficient", "insufficient", "not_applicable"]),
  role_satisfaction: z.enum(["satisfied", "unsatisfied", "not_applicable"]),
  unresolved_obligations: z.enum(["permit", "do_not_permit", "not_applicable"]),
  selection_constraint_coverage: z.enum(["satisfied", "unsatisfied"]),
  selection_constraint_basis: z.enum([
    "constraints_verified",
    "no_effect_mutation",
    "no_selection_constraints",
    "unresolved",
  ]),
  selection_constraints: z.array(selectionConstraintProofSchema).max(32),
  rationale: z.string().min(1).max(2000),
}).superRefine((verdict, ctx) => {
  const proofCount = verdict.selection_constraints.length;
  if (
    verdict.selection_constraint_coverage === "satisfied" &&
    verdict.selection_constraint_basis === "constraints_verified" &&
    proofCount === 0
  ) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["selection_constraints"],
      message: "constraints_verified requires at least one per-constraint proof",
    });
  }
  if (
    verdict.selection_constraint_coverage === "satisfied" &&
    (verdict.selection_constraint_basis === "no_effect_mutation" ||
      verdict.selection_constraint_basis === "no_selection_constraints") &&
    proofCount !== 0
  ) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["selection_constraints"],
      message: `${verdict.selection_constraint_basis} requires an empty proof list`,
    });
  }
  if (
    verdict.selection_constraint_coverage === "satisfied" &&
    verdict.selection_constraint_basis === "unresolved"
  ) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["selection_constraint_basis"],
      message: "satisfied coverage cannot have an unresolved basis",
    });
  }
});

export type TaskLedgerReviewVerdict = z.infer<typeof taskLedgerReviewVerdictSchema>;

export type ParseTaskLedgerReviewVerdict =
  | { ok: true; verdict: TaskLedgerReviewVerdict }
  | { ok: false; error: string };

export function reviewAllowsProposedAction(
  verdict: TaskLedgerReviewVerdict,
  toolName: string,
): boolean {
  if (verdict.decision === "allow") {
    return toolName !== "plasm_run" || verdict.selection_constraint_coverage === "satisfied";
  }
  if (verdict.decision === "complete_without_value") {
    return toolName === COMPLETE_TASK_TOOL_NAME;
  }
  if (verdict.decision === "submit") return toolName === SUBMIT_ANSWER_TOOL_NAME;
  return false;
}

export function parseTaskLedgerReviewVerdict(input: unknown): ParseTaskLedgerReviewVerdict {
  const parsed = taskLedgerReviewVerdictSchema.safeParse(input);
  if (!parsed.success) {
    return { ok: false, error: parsed.error.issues.map((issue) => issue.message).join("; ") };
  }
  return { ok: true, verdict: parsed.data };
}

/** Parse failure is not a domain judgment — do not execute the proposed action. */
export function continueVerdictForUnreadableReview(error: string): TaskLedgerReviewVerdict {
  return {
    decision: "continue",
    negative_evidence: "not_applicable",
    role_satisfaction: "not_applicable",
    unresolved_obligations: "not_applicable",
    selection_constraint_coverage: "unsatisfied",
    selection_constraint_basis: "unresolved",
    selection_constraints: [],
    rationale: `Reviewer output was not a structured verdict (${error}). Proposed action was not executed.`,
  };
}

export const TASK_LEDGER_REVIEW_INSTRUCTION_HEADER = "## Instruction";
export const TASK_LEDGER_REVIEW_LEDGER_HEADER = "## Current ledger snapshot";
export const TASK_LEDGER_REVIEW_ACTION_HEADER = "## Proposed next action";
export const TASK_LEDGER_REVIEW_OBSERVATIONS_HEADER = "## Recent Plasm observations";

export type TaskLedgerReviewProposedAction = {
  toolName: string;
  input: unknown;
};

export function composeTaskLedgerReviewPacket(parts: {
  instruction: string;
  ledger: TaskLedgerRecord;
  proposedAction: TaskLedgerReviewProposedAction;
  observations: string;
}): string {
  return [
    TASK_LEDGER_REVIEW_INSTRUCTION_HEADER,
    parts.instruction,
    "",
    TASK_LEDGER_REVIEW_LEDGER_HEADER,
    "```json",
    JSON.stringify(parts.ledger, null, 2),
    "```",
    "",
    TASK_LEDGER_REVIEW_ACTION_HEADER,
    "```json",
    JSON.stringify(
      { tool: parts.proposedAction.toolName, input: parts.proposedAction.input ?? {} },
      null,
      2,
    ),
    "```",
    "",
    TASK_LEDGER_REVIEW_OBSERVATIONS_HEADER,
    parts.observations,
  ].join("\n");
}

const OBSERVATION_TOOLS = new Set(["plasm", "plasm_run", "plasm_read_run_artifact"]);
const MAX_OBSERVATION_CHARS = 4000;

function partText(part: unknown): string {
  if (typeof part === "string") return part;
  if (!part || typeof part !== "object") return "";
  const rec = part as Record<string, unknown>;
  if (typeof rec.text === "string") return rec.text;
  if (typeof rec.result === "string") return rec.result;
  if (typeof rec.output === "string") return rec.output;
  const output = rec.output;
  if (output && typeof output === "object" && typeof (output as { value?: unknown }).value === "string") {
    return (output as { value: string }).value;
  }
  return "";
}

/**
 * Compact last Plasm plan/run/artifact texts. Not a transcript dump.
 * Host does not judge these strings true or false.
 */
export function extractRecentPlasmObservations(messages: readonly ModelMessage[]): string {
  const chunks: string[] = [];
  for (const message of messages) {
    if (message.role !== "tool") continue;
    const content = message.content as unknown;
    if (typeof content === "string") {
      const trimmed = content.trim();
      if (trimmed) chunks.push(trimmed);
      continue;
    }
    if (!Array.isArray(content)) continue;
    for (const part of content) {
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      const name = typeof rec.toolName === "string" ? rec.toolName : "";
      if (name && !OBSERVATION_TOOLS.has(name)) continue;
      const text = partText(part).trim();
      if (!text) continue;
      chunks.push(name ? `**${name}**\n${text}` : text);
    }
  }
  if (chunks.length === 0) {
    return "(no plasm plan or run observations in recent steps)";
  }
  const joined = chunks.slice(-3).join("\n\n");
  if (joined.length <= MAX_OBSERVATION_CHARS) return joined;
  return joined.slice(joined.length - MAX_OBSERVATION_CHARS);
}

function proposedRunRef(proposed: TaskLedgerReviewProposedAction): string | null {
  if (proposed.toolName !== "plasm_run" || !proposed.input || typeof proposed.input !== "object") {
    return null;
  }
  const runRef = (proposed.input as { run_ref?: unknown }).run_ref;
  return typeof runRef === "string" && runRef.length > 0 ? runRef : null;
}

/** Bind mutation review to the exact dry-plan result that minted its run_ref. */
export function extractReviewObservations(
  messages: readonly ModelMessage[],
  proposed: TaskLedgerReviewProposedAction,
): string {
  const runRef = proposedRunRef(proposed);
  if (runRef === null) return extractRecentPlasmObservations(messages);

  const marker = `pass \`run_ref\`: \`${runRef}\``;
  for (let messageIndex = messages.length - 1; messageIndex >= 0; messageIndex -= 1) {
    const message = messages[messageIndex];
    if (message?.role !== "tool" || !Array.isArray(message.content)) continue;
    for (let partIndex = message.content.length - 1; partIndex >= 0; partIndex -= 1) {
      const part = message.content[partIndex];
      if (!part || typeof part !== "object") continue;
      const rec = part as Record<string, unknown>;
      if (rec.toolName !== "plasm") continue;
      const text = partText(part).trim();
      if (!text.includes(marker)) continue;
      const bounded = text.length <= MAX_OBSERVATION_CHARS
        ? text
        : text.slice(text.length - MAX_OBSERVATION_CHARS);
      return `**plasm plan for ${runRef}**\n${bounded}`;
    }
  }
  return `(no plasm dry-plan observation matched proposed run_ref ${runRef})`;
}

export function formatReviewContinueError(
  toolName: string,
  verdict: TaskLedgerReviewVerdict,
): string {
  return [
    `ledger review: ${verdict.decision} — proposed ${toolName} was not executed.`,
    JSON.stringify(verdict),
  ].join("\n");
}

export function emptyReviewUsage(): LanguageModelUsage {
  return {
    inputTokens: 0,
    outputTokens: 0,
    totalTokens: 0,
    inputTokenDetails: { noCacheTokens: 0, cacheReadTokens: 0, cacheWriteTokens: 0 },
    outputTokenDetails: { textTokens: 0, reasoningTokens: 0 },
  };
}

export function addLanguageModelUsage(a: LanguageModelUsage, b: LanguageModelUsage): LanguageModelUsage {
  const sum = (x: number | undefined, y: number | undefined) =>
    x === undefined || y === undefined ? undefined : x + y;
  return {
    inputTokens: sum(a.inputTokens, b.inputTokens),
    outputTokens: sum(a.outputTokens, b.outputTokens),
    totalTokens: sum(a.totalTokens, b.totalTokens),
    inputTokenDetails: {
      noCacheTokens: sum(a.inputTokenDetails.noCacheTokens, b.inputTokenDetails.noCacheTokens),
      cacheReadTokens: sum(a.inputTokenDetails.cacheReadTokens, b.inputTokenDetails.cacheReadTokens),
      cacheWriteTokens: sum(a.inputTokenDetails.cacheWriteTokens, b.inputTokenDetails.cacheWriteTokens),
    },
    outputTokenDetails: {
      textTokens: sum(a.outputTokenDetails.textTokens, b.outputTokenDetails.textTokens),
      reasoningTokens: sum(a.outputTokenDetails.reasoningTokens, b.outputTokenDetails.reasoningTokens),
    },
  };
}

export type TaskLedgerReviewRecord = {
  proposed: TaskLedgerReviewProposedAction;
  verdict: TaskLedgerReviewVerdict;
  allowed: boolean;
  rawText: string;
  usage: LanguageModelUsage;
  packet: string;
};

export async function generateTaskLedgerReview(opts: {
  model: LanguageModel;
  instruction: string;
  ledger: TaskLedgerRecord;
  proposedAction: TaskLedgerReviewProposedAction;
  observations: string;
}): Promise<Omit<TaskLedgerReviewRecord, "allowed">> {
  const packet = composeTaskLedgerReviewPacket({
    instruction: opts.instruction,
    ledger: opts.ledger,
    proposedAction: opts.proposedAction,
    observations: opts.observations,
  });
  const reviewTools = {
    [TASK_LEDGER_REVIEW_SUBMIT_TOOL_NAME]: tool({
      description: "Submit the one structured review verdict.",
      inputSchema: taskLedgerReviewVerdictSchema,
    }),
  };
  const streamResult = streamText({
    model: opts.model,
    system: TASK_LEDGER_REVIEW_LITURGY,
    messages: [{ role: "user", content: packet }],
    tools: reviewTools,
    toolChoice: {
      type: "tool",
      toolName: TASK_LEDGER_REVIEW_SUBMIT_TOOL_NAME,
    },
    stopWhen: stepCountIs(1),
    temperature: 0,
    maxOutputTokens: 16_384,
  });
  const [text, usage, toolCalls] = await Promise.all([
    streamResult.text,
    streamResult.usage,
    streamResult.toolCalls,
  ]);
  const submitted = toolCalls.find(
    (call) => call.toolName === TASK_LEDGER_REVIEW_SUBMIT_TOOL_NAME,
  );
  const parsed = submitted
    ? parseTaskLedgerReviewVerdict(submitted.input)
    : { ok: false as const, error: "reviewer did not call submit_task_ledger_review" };
  const verdict = parsed.ok ? parsed.verdict : continueVerdictForUnreadableReview(parsed.error);
  const rawText = submitted ? JSON.stringify(submitted.input) : text;
  return { proposed: opts.proposedAction, verdict, rawText, usage, packet };
}

export type TaskLedgerReviewSeatDeps = {
  model: LanguageModel;
  getInstruction: () => string;
  getLedger: () => TaskLedgerRecord;
  getMessages: () => readonly ModelMessage[];
};

/** One review seat: one isolated generate per gated proposal. */
export class TaskLedgerReviewSeat {
  generateCount = 0;
  usage: LanguageModelUsage = emptyReviewUsage();
  records: TaskLedgerReviewRecord[] = [];

  constructor(private readonly deps: TaskLedgerReviewSeatDeps) {}

  reset(): void {
    this.generateCount = 0;
    this.usage = emptyReviewUsage();
    this.records = [];
  }

  async review(proposed: TaskLedgerReviewProposedAction): Promise<TaskLedgerReviewRecord> {
    this.generateCount += 1;
    const generated = await generateTaskLedgerReview({
      model: this.deps.model,
      instruction: this.deps.getInstruction(),
      ledger: this.deps.getLedger(),
      proposedAction: proposed,
      observations: extractReviewObservations(this.deps.getMessages(), proposed),
    });
    const allowed = reviewAllowsProposedAction(generated.verdict, proposed.toolName);
    const record: TaskLedgerReviewRecord = { ...generated, allowed };
    this.records.push(record);
    this.usage = addLanguageModelUsage(this.usage, generated.usage);
    return record;
  }
}

/**
 * Full cutover when the experiment is on: every gated execute goes through
 * one review seat. Host does not inspect remaining_work.
 */
export function wrapTaskLedgerReviewTools(tools: ToolSet, seat: TaskLedgerReviewSeat): ToolSet {
  const next: ToolSet = { ...tools };
  for (const name of TASK_LEDGER_REVIEW_GATED_TOOLS) {
    const existing = next[name];
    if (!existing || typeof existing !== "object") continue;
    const original = (existing as { execute?: (...args: never[]) => unknown }).execute;
    if (typeof original !== "function") continue;
    next[name] = {
      ...existing,
      execute: async (...args: never[]) => {
        const record = await seat.review({ toolName: name, input: args[0] });
        if (record.allowed) {
          return original.apply(existing, args);
        }
        throw new Error(formatReviewContinueError(name, record.verdict));
      },
    } as ToolSet[string];
  }
  return next;
}

export function snapshotOrEmptyLedger(record: TaskLedgerRecord | null | undefined): TaskLedgerRecord {
  return record ?? emptyTaskLedger();
}

/**
 * Isolated review liturgy. Domain-general. Named composition only.
 * No grader, no eval internals, no AppWorld coaching, no task recipes.
 */
export const TASK_LEDGER_REVIEW_LITURGY = [
  "Review one proposed action for decision-consistency against the instruction, the model-authored ledger snapshot, and recent Plasm observations.",
  "",
  "The host does not invent obligations and does not decide domain correctness. Submit exactly one verdict through `submit_task_ledger_review`.",
  "",
  "For selection constraints, return `selection_constraint_coverage`, `selection_constraint_basis`, and `selection_constraints`. Each proof entry names one original constraint, its acquired evidence, and the visible dataflow by which that evidence narrows the reviewed mutation set.",
  "",
  "decision selects the proposed action. A plasm_run is executable only when coverage is satisfied. complete_without_value executes only complete_task. submit executes only submit_answer. continue and unresolved do not execute.",
  "",
  "Judge these four consistencies without inventing domain facts:",
  "1. Whether negative evidence is sufficient for the claims it supports, including whether a search was exhaustive or targeted.",
  "2. Whether completed operations satisfy the requested roles in the instruction.",
  "3. Whether unresolved obligations permit the proposed action.",
  "4. For a proposed plasm_run that applies a requested effect, enumerate every original selection constraint that narrows which records receive that effect. Every such constraint must have acquired evidence, and the exact reviewed plan must visibly constrain the mutation set through dataflow from that evidence. Use `constraints_verified` with one proof entry per constraint. Use `no_effect_mutation` with an empty list only when the exact proposed plan performs prerequisite acquisition or session setup but applies no requested effect. Use `no_selection_constraints` with an empty list only when the plan applies a requested effect and the original instruction has no selection constraints; these cases are vacuously satisfied. Otherwise use unresolved/unsatisfied and choose continue. Merely mentioning a constraint, recording an interpretation, or performing an independent read that does not feed the mutation is not sufficient. If the observations do not expose the exact plan for the proposed run_ref, choose continue.",
  "",
  "A nonempty remaining_work list is not automatic failure. Do not invent a new success path.",
].join("\n");

/**
 * Actor-facing named section when the review experiment is on.
 * Not a completion contract and not a remaining_work auto-fail.
 */
export const TASK_LEDGER_REVIEW_ACTOR_LITURGY = [
  "## Ledger review",
  "",
  "Consequential `plasm_run` and finish proposals (`complete_task` / `submit_answer`) are reviewed in a separate seat against the instruction, the current ledger snapshot, and recent Plasm observations. The host does not invent obligations or judge the domain. A review that does not allow the proposal returns a tool error; that is not completion.",
  "",
  "Before proposing a `plasm_run` that applies a requested effect, acquire evidence for all original selection constraints and make that evidence constrain the mutation set through program dataflow. Mentioning a constraint or performing an unrelated lookup does not satisfy this invariant. Prerequisite acquisition and session-setup plans may run before that proof exists when they apply no requested effect.",
].join("\n");
