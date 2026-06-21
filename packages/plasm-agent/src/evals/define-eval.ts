export const PLASM_EVAL_KIND = "eval" as const;

export type EvalToolName =
  | "discover_capabilities"
  | "plasm_context"
  | "plasm"
  | "plasm_run"
  | "read_skill"
  | "delegate_subagent";

export interface EvalAssert {
  /** Tools that must appear in at least one step (live agent loop). */
  toolsUsedAny?: EvalToolName[];
  /** Final assistant text must match. */
  responseIncludes?: string | RegExp;
  /** Minimum tool-calling steps observed. */
  minSteps?: number;
}

export interface EvalDefinition {
  readonly __plasmSlotKind: typeof PLASM_EVAL_KIND;
  name: string;
  /** Natural-language goal sent to the live agent loop. */
  goal: string;
  assert: EvalAssert;
}

export interface DefineEvalInput {
  name: string;
  goal: string;
  assert: EvalAssert;
}

/** Live LLM eval — requires AI Gateway; fixture-only chains are not supported. */
export function defineEval(input: DefineEvalInput): EvalDefinition {
  if (!input.name?.trim()) {
    throw new Error("defineEval: name is required");
  }
  if (!input.goal?.trim()) {
    throw new Error("defineEval: goal is required");
  }
  return Object.freeze({
    __plasmSlotKind: PLASM_EVAL_KIND,
    name: input.name.trim(),
    goal: input.goal.trim(),
    assert: input.assert,
  });
}

export function isEvalDefinition(value: unknown): value is EvalDefinition {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as EvalDefinition).__plasmSlotKind === PLASM_EVAL_KIND
  );
}
