import { wrapLanguageModel, type LanguageModel } from "ai";

/** Admit a generated batch only when its provider stream has terminated.
 * The SDK dispatches tools on finish, which can precede EOF. Holding only that
 * marker keeps stalled provider tails inside the generation deadline without
 * allowing effects whose step audit cannot yet be finalized.
 */
export function withProviderCompletion(model: LanguageModel) {
  // streamText resolves model IDs before invoking prepareStep.
  if (typeof model === "string") throw new Error("Expected an SDK-resolved language model");
  return wrapLanguageModel({
    model,
    middleware: {
      wrapStream: async ({ doStream }) => {
        const result = await doStream();
        const reader = result.stream.getReader();
        type Part = NonNullable<Awaited<ReturnType<typeof reader.read>>["value"]>;
        let finish: Extract<Part, { type: "finish" }> | undefined;
        return {
          ...result,
          stream: new ReadableStream<Part>({
            async pull(controller) {
              for (;;) {
                const next = await reader.read();
                if (next.done) {
                  if (finish) controller.enqueue(finish);
                  controller.close();
                  reader.releaseLock();
                  return;
                }
                if (next.value.type === "finish") {
                  finish = next.value;
                } else {
                  controller.enqueue(next.value);
                  return;
                }
              }
            },
            async cancel(reason) {
              await reader.cancel(reason);
              reader.releaseLock();
            },
          }),
        };
      },
    },
  });
}
