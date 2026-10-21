import { tool, type ToolSet } from "ai";
import { z } from "zod";

import { toolInput } from "./tool-input.js";

export const TASK_LEDGER_TOOL_NAME = "task_ledger";

/** Compact JSON budget for one replace. Per-field caps alone are not enough. */
export const TASK_LEDGER_MAX_BYTES = 4096;

const TEXT = z.string().min(1).max(2000);
const OPTIONAL_TEXT = z.string().max(4000);
// One grammar drives admission and the advertised JSON Schema pattern.
const EVIDENCE_REF_PATTERN = /^(?:pr[a-f0-9]{64}|p[cg]\d+|plasm:\/\/\S+|plasm|plasm_run|plasm_context|plasm_failure)(?![\s\S])/;
const EVIDENCE_REF_DESCRIPTION =
  "Copy a bare reference: pr followed by exactly 64 lowercase hex digits, pc followed by digits (review), pg followed by digits (page), a plasm:// URI, or plasm/plasm_run/plasm_context/plasm_failure. No labels or surrounding whitespace. Syntax is checked, not existence or truth.";
const evidenceRefSchema = z.string().min(1).max(500)
  .regex(EVIDENCE_REF_PATTERN, EVIDENCE_REF_DESCRIPTION)
  .describe(EVIDENCE_REF_DESCRIPTION);

/**
 * Host checks citation syntax only, not existence or provenance.
 * It does not treat the citation as proof of the model's reading.
 */
export function isKnownEvidenceRef(raw: string): boolean {
  return evidenceRefSchema.safeParse(raw).success;
}

const factClaimSchema = z.object({
  kind: z.literal("fact"),
  text: TEXT,
  evidence_ref: evidenceRefSchema,
});

const interpretationClaimSchema = z.object({
  kind: z.literal("interpretation"),
  text: TEXT,
  evidence_ref: evidenceRefSchema.optional(),
});

const alternativeSchema = z.object({
  condition: TEXT.describe("User-stated condition that authorizes this alternative"),
  action: TEXT,
  condition_met: z
    .enum(["yes", "no", "unknown"])
    .describe("Model interpretation — not host-verified"),
});

const completedEffectSchema = z.object({
  effect: TEXT,
  evidence_ref: evidenceRefSchema.optional(),
  satisfaction: z
    .literal("interpretation")
    .describe("Completion claims stay model interpretations"),
  note: OPTIONAL_TEXT.optional(),
});

export const taskLedgerRecordSchema = z.object({
  requested_outcomes: z.array(TEXT).max(32),
  scope: OPTIONAL_TEXT,
  alternative_conditions: z.array(alternativeSchema).max(32),
  observed_facts: z.array(factClaimSchema).max(64),
  interpretations: z.array(interpretationClaimSchema).max(64),
  unresolved_questions: z.array(TEXT).max(32),
  completed_effects: z.array(completedEffectSchema).max(64),
  remaining_work: z.array(TEXT).max(32),
});

export type TaskLedgerRecord = z.infer<typeof taskLedgerRecordSchema>;

export const taskLedgerInputSchema = z.object({
  action: z.enum(["read", "write"]),
  record: taskLedgerRecordSchema.optional(),
});

export function emptyTaskLedger(): TaskLedgerRecord {
  return {
    requested_outcomes: [],
    scope: "",
    alternative_conditions: [],
    observed_facts: [],
    interpretations: [],
    unresolved_questions: [],
    completed_effects: [],
    remaining_work: [],
  };
}

export function taskLedgerRecordBytes(record: TaskLedgerRecord): number {
  return Buffer.byteLength(JSON.stringify(record), "utf8");
}

export type ParseTaskLedgerResult =
  | { ok: true; record: TaskLedgerRecord }
  | { ok: false; error: string };

export function parseTaskLedgerRecord(input: unknown): ParseTaskLedgerResult {
  const parsed = taskLedgerRecordSchema.safeParse(input);
  if (!parsed.success) {
    return { ok: false, error: parsed.error.issues.map((issue) => issue.message).join("; ") };
  }
  const bytes = taskLedgerRecordBytes(parsed.data);
  if (bytes > TASK_LEDGER_MAX_BYTES) {
    return {
      ok: false,
      error: `record exceeds aggregate budget (${bytes} > ${TASK_LEDGER_MAX_BYTES} bytes)`,
    };
  }
  return { ok: true, record: structuredClone(parsed.data) };
}

/** Host store: persist model writes only. Never invent obligations or judge correctness. */
export class TaskLedgerStore {
  private current: TaskLedgerRecord = emptyTaskLedger();

  get record(): TaskLedgerRecord {
    return structuredClone(this.current);
  }

  replace(record: TaskLedgerRecord): TaskLedgerRecord {
    this.current = structuredClone(record);
    return this.record;
  }

  clear(): void {
    this.current = emptyTaskLedger();
  }
}

export const TASK_LEDGER_STATE_HEADER = "## Model-authored task ledger";

/** Per-step host echo. Not system law and not a completion contract. */
export function renderTaskLedgerState(record: TaskLedgerRecord): string {
  return [
    TASK_LEDGER_STATE_HEADER,
    "",
    "Host echo of your last accepted `task_ledger` write. This is model-authored state, not host law and not a completion contract. No fields were added. The host does not treat citations as proof.",
    "",
    "```json",
    JSON.stringify(record, null, 2),
    "```",
  ].join("\n");
}

function formatLedgerToolResult(action: "read" | "write", record: TaskLedgerRecord): string {
  const verb =
    action === "write" ? "Host stored your write. No fields were added." : "Host echo. No fields were added.";
  return `${verb}\n\n\`\`\`json\n${JSON.stringify(record, null, 2)}\n\`\`\``;
}

export const TASK_LEDGER_TOOL_DESCRIPTION =
  "Host-stored compact record for this instruction: requested outcomes and scope, " +
  "conditions that authorize alternatives, observed facts, interpretations, " +
  "completed effects with evidence refs, unresolved questions, and remaining work. " +
  "`write` replaces the whole record; `read` echoes it. " +
  `The host rejects writes over ${TASK_LEDGER_MAX_BYTES} bytes (JSON). ` +
  "The host stores what you send. It does not invent outcomes or remaining work, " +
  "and it does not decide that an alternative is authorized or that the instruction is satisfied. " +
  "`observed_facts` require `evidence_ref`. " + EVIDENCE_REF_DESCRIPTION + " " +
  "The host checks that the citation is a known shape; it does not treat it as proof. " +
  "`interpretations` and `completed_effects.satisfaction` are your readings. " +
  "A failed preferred action is not the user condition for an alternative. " +
  "Not a terminal.";

export function createTaskLedgerTool(store: TaskLedgerStore): ToolSet {
  return {
    [TASK_LEDGER_TOOL_NAME]: tool({
      description: TASK_LEDGER_TOOL_DESCRIPTION,
      inputSchema: toolInput(taskLedgerInputSchema),
      execute: async (input: z.infer<typeof taskLedgerInputSchema>) => {
        if (input.action === "read") {
          return formatLedgerToolResult("read", store.record);
        }
        if (input.record === undefined) {
          return "**task_ledger** write rejected: record is required for write. Host store unchanged.";
        }
        const parsed = parseTaskLedgerRecord(input.record);
        if (!parsed.ok) {
          return `**task_ledger** write rejected: ${parsed.error}\nHost store unchanged.`;
        }
        return formatLedgerToolResult("write", store.replace(parsed.record));
      },
    }),
  };
}
