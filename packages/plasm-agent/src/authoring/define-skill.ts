export const PLASM_SKILL_KIND = "skill" as const;

export interface SkillDefinition {
  readonly __plasmSlotKind: typeof PLASM_SKILL_KIND;
  name: string;
  description?: string;
  body: string;
}

export interface DefineSkillInput {
  name: string;
  description?: string;
  body: string;
}

/** Eve-shaped skill definition (markdown body for progressive disclosure). */
export function defineSkill(input: DefineSkillInput): SkillDefinition {
  if (!input.name?.trim()) {
    throw new Error("defineSkill: name is required");
  }
  if (!input.body?.trim()) {
    throw new Error("defineSkill: body is required");
  }
  return Object.freeze({
    __plasmSlotKind: PLASM_SKILL_KIND,
    name: input.name.trim(),
    description: input.description?.trim(),
    body: input.body,
  });
}

export function isSkillDefinition(value: unknown): value is SkillDefinition {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as SkillDefinition).__plasmSlotKind === PLASM_SKILL_KIND
  );
}
