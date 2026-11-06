import { wrapLanguageModel, type LanguageModel } from "ai";

export interface ProviderRequestEvent {
  attempt: number;
  phase: "start" | "first_chunk" | "finish" | "end" | "abort" | "cancel" | "error";
  at_ms: number;
  prompt_bytes?: number;
  system_bytes?: number;
  tool_schema_bytes?: number;
  prompt_messages?: number;
  tool_count?: number;
  max_output_tokens?: number;
  temperature?: number;
  chunk_type?: string;
  finish_reason?: string;
  input_tokens?: number;
  output_tokens?: number;
  reasoning_tokens?: number;
  tool_schema_sizes?: Array<{ name: string; bytes: number }>;
}

export type ProviderRequestObserver = (event: ProviderRequestEvent) => void;

/** Stop awaiting an upstream operation even if its implementation ignores abort. */
async function abortable<T>(operation: PromiseLike<T>, signal?: AbortSignal): Promise<T> {
  if (!signal) return await operation;
  if (signal.aborted) throw signal.reason;
  return await new Promise<T>((resolve, reject) => {
    const abort = () => { signal.removeEventListener("abort", abort); reject(signal.reason); };
    signal.addEventListener("abort", abort, { once: true });
    Promise.resolve(operation).then(
      value => { signal.removeEventListener("abort", abort); resolve(value); },
      error => { signal.removeEventListener("abort", abort); reject(error); },
    );
    if (signal.aborted) abort();
  });
}

/** Admit a generated batch only when its provider stream has terminated.
 * The SDK dispatches tools on finish, which can precede EOF. Holding only that
 * marker keeps stalled provider tails inside the generation deadline without
 * allowing effects whose step audit cannot yet be finalized.
 */
export function withProviderCompletion(model: LanguageModel, observer?: ProviderRequestObserver) {
  // streamText resolves model IDs before invoking prepareStep.
  if (typeof model === "string") throw new Error("Expected an SDK-resolved language model");
  let attemptCounter = 0;
  return wrapLanguageModel({
    model,
    middleware: {
      wrapStream: async ({ doStream, params }) => {
        const attempt = ++attemptCounter;
        const emit = (event: Omit<ProviderRequestEvent, "at_ms" | "attempt">) => {
          try { observer?.({ ...event, at_ms: Date.now(), attempt }); } catch { /* Observability cannot affect execution. */ }
        };
        if (observer) {
          try {
            const size = (value: unknown) => Buffer.byteLength(JSON.stringify(value ?? null), "utf8");
            emit({
              phase: "start",
              prompt_bytes: size(params.prompt),
              system_bytes: size(params.prompt.filter(message => message.role === "system")),
              tool_schema_bytes: size(params.tools),
              prompt_messages: params.prompt.length,
              tool_count: params.tools?.length ?? 0,
              tool_schema_sizes: params.tools?.map(item => ({ name: item.name, bytes: size(item) })) ?? [],
              max_output_tokens: params.maxOutputTokens,
              temperature: params.temperature,
            });
          } catch {
            emit({ phase: "start" });
          }
        }
        const abort = () => emit({ phase: "abort" });
        params.abortSignal?.addEventListener("abort", abort, { once: true });
        if (params.abortSignal?.aborted) {
          abort();
          params.abortSignal.removeEventListener("abort", abort);
          throw params.abortSignal.reason;
        }
        let result: Awaited<ReturnType<typeof doStream>>;
        const upstream = Promise.resolve().then(() => {
          if (params.abortSignal?.aborted) throw params.abortSignal.reason;
          return doStream();
        });
        void upstream.then(late => {
          if (params.abortSignal?.aborted) void late.stream.cancel(params.abortSignal.reason).catch(() => {});
        }, () => {});
        try { result = await abortable(upstream, params.abortSignal); }
        catch (error) {
          emit({ phase: "error" });
          params.abortSignal?.removeEventListener("abort", abort);
          throw error;
        }
        const reader = result.stream.getReader();
        type Part = NonNullable<Awaited<ReturnType<typeof reader.read>>["value"]>;
        let finish: Extract<Part, { type: "finish" }> | undefined;
        let first = false;
        return {
          ...result,
          stream: new ReadableStream<Part>({
            async pull(controller) {
              for (;;) {
                let next: Awaited<ReturnType<typeof reader.read>>;
                try { next = await abortable(reader.read(), params.abortSignal); }
                catch (error) {
                  emit({ phase: "error" });
                  params.abortSignal?.removeEventListener("abort", abort);
                  void reader.cancel(error).catch(() => {}).finally(() => {
                    try { reader.releaseLock(); } catch { /* An upstream read may still be pending. */ }
                  });
                  throw error;
                }
                if (next.done) {
                  emit({ phase: "end" });
                  params.abortSignal?.removeEventListener("abort", abort);
                  if (finish) controller.enqueue(finish);
                  controller.close();
                  reader.releaseLock();
                  return;
                }
                if (!first && !["stream-start", "response-metadata"].includes(next.value.type)) {
                  first = true;
                  emit({ phase: "first_chunk", chunk_type: next.value.type });
                }
                if (next.value.type === "finish") {
                  emit({
                    phase: "finish",
                    finish_reason: next.value.finishReason.unified,
                    input_tokens: next.value.usage.inputTokens.total,
                    output_tokens: next.value.usage.outputTokens.total,
                    reasoning_tokens: next.value.usage.outputTokens.reasoning,
                  });
                  finish = next.value;
                } else {
                  controller.enqueue(next.value);
                  return;
                }
              }
            },
            async cancel(reason) {
              emit({ phase: "cancel" });
              params.abortSignal?.removeEventListener("abort", abort);
              await reader.cancel(reason);
              reader.releaseLock();
            },
          }),
        };
      },
    },
  });
}
