import { experimental_setAttributes } from "workflow";

import {
  stringifyEveWorkflowAttributes,
  type EveWorkflowAttributeInput,
} from "./eve-workflow-tags.js";

let warnedTagFailure = false;

/** Write `$eve.*` tags on the current workflow run (inside `'use workflow'` or `'use step'`). */
export async function setEveWorkflowAttributes(
  attrs: EveWorkflowAttributeInput,
): Promise<void> {
  const payload = stringifyEveWorkflowAttributes(attrs);
  if (Object.keys(payload).length === 0) return;
  try {
    await experimental_setAttributes(payload, { allowReservedAttributes: true });
  } catch (err) {
    if (!warnedTagFailure) {
      warnedTagFailure = true;
      console.warn("[plasm-agent] setEveWorkflowAttributes failed; suppressing further warnings", {
        keys: Object.keys(payload),
        error: err instanceof Error ? err.message : String(err),
      });
    }
  }
}
