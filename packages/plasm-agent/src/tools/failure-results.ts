import { randomUUID } from "node:crypto";
import type { ExecutionFailure } from "../runtime/execution-failure.js";

function sourceScope(path: readonly string[]): string {
  const label = path.flatMap(part => part.split("/")).filter(part =>
    part && !/^__(?:plasm_|py|scope)/.test(part) && !/^return_\d+$/.test(part)).join("/");
  return label ? ` at ${label}` : "";
}

/** Agent-readable recorded outcomes, not raw transport operands or host metadata. */
export class FailureResults {
  private readonly records = new Map<string, readonly string[]>();

  record(failure: ExecutionFailure): string {
    const ref = `failure_${randomUUID()}`;
    const lines: string[] = [];
    if (failure.input_occurrence) {
      const input = failure.input_occurrence;
      lines.push(`Primary failure input occurrence [${input.occurrence_path.join("/")}]${sourceScope(input.scope_path)}: ${input.source_identity ?? "identity unavailable"}`);
    }
    for (const effect of failure.effects) {
      lines.push(`${effect.entry_id}/${effect.capability}: ${effect.completed} completed; ${effect.failed} failed. A failed invocation does not establish absence of an effect.`);
      for (const occurrence of effect.occurrences) {
        lines.push(`${effect.entry_id}/${effect.capability} — ${occurrence.status} — occurrence [${occurrence.occurrence_path.join("/")}] — ${occurrence.source_identity ?? "identity unavailable"}${sourceScope(occurrence.scope_path)}`);
      }
    }
    for (const dispatch of failure.dispatches) {
      const origin = dispatch.resolved_effect?.origin;
      const coordinate = origin ? ` at occurrence [${origin.occurrence_path.join("/")}]` : "";
      lines.push(`${dispatch.operation.entry_id}/${dispatch.operation.capability}${coordinate}: ${dispatch.status === "unresolved" ? "request sent; no response received; effect unknown" : "response received; this dispatch is not an additional effect receipt"}`);
    }
    if (failure.effects_unresolved) lines.push("Some effects have unresolved outcomes.");
    lines.push("Only recorded attempts are listed. This is not an exhaustive list of unattempted work. Earlier successful operations were not rolled back.");
    this.records.set(ref, lines);
    // Bound per-runtime storage. Eviction is explicit to readers, never a live retry.
    if (this.records.size > 128) this.records.delete(this.records.keys().next().value!);
    return ref;
  }

  read(ref: string, offset: number): string {
    const lines = this.records.get(ref);
    if (!lines) throw new Error("Failure result is unknown or expired on this runtime. No operation was executed.");
    if (!Number.isSafeInteger(offset) || offset < 0 || offset >= lines.length) throw new Error("Failure result offset is outside the recorded result.");
    const end = Math.min(offset + 40, lines.length);
    return [
      `Recorded failure details (${offset + 1}–${end} of ${lines.length} entries).`,
      ...lines.slice(offset, end),
      end < lines.length ? `More: call plasm_failure with failure_ref=${ref} and offset=${end}.` : "End of recorded failure details.",
    ].join("\n");
  }
}

// Tool sets may be recreated between turns; evidence stays with their runtime.
const stores = new WeakMap<object, FailureResults>();
export function failureResultsFor(runtime: object): FailureResults {
  let store = stores.get(runtime);
  if (!store) { store = new FailureResults(); stores.set(runtime, store); }
  return store;
}
