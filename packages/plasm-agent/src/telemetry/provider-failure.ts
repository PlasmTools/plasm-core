import { APICallError, RetryError } from "ai";

/** Only typed provider failures qualify. Never reinterpret tool/schema errors or
 * expose provider request bodies, credentials or prompts in a recovery message.
 */
export function transientProviderFailure(error: unknown): string | undefined {
  const cause = RetryError.isInstance(error) ? error.lastError : error;
  if (!APICallError.isInstance(cause)) return undefined;
  // The OpenAI-compatible SDK wraps a failed 2xx response body stream as a
  // non-retryable APICallError. The HTTP request succeeded, but generation did
  // not: the loop may spend one ordinary step on recovery if no tool started.
  if (
    cause.statusCode !== undefined &&
    cause.statusCode >= 200 && cause.statusCode < 300 &&
    cause.message === "Failed to process successful response"
  ) {
    return "provider_response_stream_error";
  }
  if (!cause.isRetryable) return undefined;
  if (cause.statusCode === undefined) return "provider_transport_error";
  if ([408, 429, 500, 502, 503, 504, 520, 521, 522, 523, 524].includes(cause.statusCode)) {
    return `provider_http_${cause.statusCode}`;
  }
  return undefined;
}
