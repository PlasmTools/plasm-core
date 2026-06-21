import type { AuthoringContext } from "./context.js";
import type { HookDefinition, HookEvent } from "./define-hook.js";

export class HookRunner {
  constructor(private readonly hooks: HookDefinition[]) {}

  async emit(
    event: HookEvent,
    ctx: AuthoringContext,
    detail?: Record<string, unknown>,
  ): Promise<void> {
    for (const hook of this.hooks) {
      const events = Array.isArray(hook.on) ? hook.on : [hook.on];
      if (!events.includes(event)) continue;
      await hook.handler(ctx, detail);
    }
  }
}
