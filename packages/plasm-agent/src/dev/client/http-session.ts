import type { ProjectInfoPayload } from "../../project-info.js";

export type SessionEventType = "turn:start" | "turn:step" | "turn:finish" | "turn:error";

export interface ParsedSessionEvent {
  id?: string;
  type: SessionEventType;
  at?: string;
  message?: string;
  toolsUsed?: string[];
  text?: string | null;
  steps?: number;
  usage?: unknown;
  finishReason?: string | null;
}

export interface SessionTurnResponse {
  sessionId: string;
  continuationToken: string;
  status?: string;
  text?: string;
  steps?: number;
  usage?: unknown;
  error?: string;
  message?: string;
}

export interface DevSessionRef {
  sessionId: string;
  continuationToken: string;
}

export interface SendTurnOptions {
  /** When false, POST returns immediately; attach SSE for live step events. Default true. */
  wait?: boolean;
  onEvent?: (event: ParsedSessionEvent) => void;
}

function parseSseBlock(block: string): ParsedSessionEvent | null {
  const lines = block.split("\n");
  let eventType: SessionEventType | undefined;
  let data = "";
  for (const line of lines) {
    if (line.startsWith("event: ")) eventType = line.slice(7).trim() as SessionEventType;
    if (line.startsWith("data: ")) data += line.slice(6);
  }
  if (!eventType || !data) return null;
  try {
    const parsed = JSON.parse(data) as Record<string, unknown>;
    return {
      id: typeof parsed.id === "string" ? parsed.id : undefined,
      type: eventType,
      at: typeof parsed.at === "string" ? parsed.at : undefined,
      message: typeof parsed.message === "string" ? parsed.message : undefined,
      toolsUsed: Array.isArray(parsed.toolsUsed)
        ? parsed.toolsUsed.filter((t): t is string => typeof t === "string")
        : undefined,
      text: typeof parsed.text === "string" ? parsed.text : parsed.text === null ? null : undefined,
      steps: typeof parsed.steps === "number" ? parsed.steps : undefined,
      usage: parsed.usage,
      finishReason:
        typeof parsed.finishReason === "string"
          ? parsed.finishReason
          : parsed.finishReason === null
            ? null
            : undefined,
    };
  } catch {
    return null;
  }
}

export async function consumeSessionStream(
  streamUrl: string,
  onEvent: (event: ParsedSessionEvent) => void,
  signal?: AbortSignal,
): Promise<ParsedSessionEvent | null> {
  const res = await fetch(streamUrl, { signal });
  if (!res.ok || !res.body) {
    throw new Error(`stream ${res.status}`);
  }
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let terminal: ParsedSessionEvent | null = null;
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    const blocks = buffer.split("\n\n");
    buffer = blocks.pop() ?? "";
    for (const block of blocks) {
      const event = parseSseBlock(block);
      if (!event) continue;
      onEvent(event);
      if (event.type === "turn:finish" || event.type === "turn:error") {
        terminal = event;
      }
    }
  }
  if (buffer.trim()) {
    const event = parseSseBlock(buffer);
    if (event) {
      onEvent(event);
      if (event.type === "turn:finish" || event.type === "turn:error") terminal = event;
    }
  }
  return terminal;
}

export class DevHttpSessionClient {
  constructor(readonly baseUrl: string) {}

  async fetchInfo(): Promise<ProjectInfoPayload> {
    const res = await fetch(`${this.baseUrl}/plasm/v1/info`);
    if (!res.ok) throw new Error(`info ${res.status}`);
    return res.json() as Promise<ProjectInfoPayload>;
  }

  async sendTurn(
    message: string,
    session: DevSessionRef | null,
    options: SendTurnOptions = {},
  ): Promise<{ session: DevSessionRef; response: SessionTurnResponse }> {
    const wait = options.wait ?? true;
    const path = session
      ? `${this.baseUrl}/plasm/v1/session/${session.sessionId}`
      : `${this.baseUrl}/plasm/v1/session`;
    const body = session
      ? { message, continuationToken: session.continuationToken, wait }
      : { message, wait };

    if (!wait) {
      const postRes = await fetch(path, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(body),
      });
      const json = (await postRes.json()) as SessionTurnResponse;
      if (!json.sessionId || !json.continuationToken) {
        throw new Error(json.message ?? json.error ?? `session ${postRes.status}`);
      }
      const ref = { sessionId: json.sessionId, continuationToken: json.continuationToken };
      if (postRes.status >= 400) {
        return { session: ref, response: json };
      }
      const terminal = await consumeSessionStream(
        `${this.baseUrl}/plasm/v1/session/${ref.sessionId}/stream`,
        (ev) => options.onEvent?.(ev),
      );
      const response: SessionTurnResponse = {
        ...json,
        text: terminal?.text ?? json.text,
        steps: terminal?.steps ?? json.steps,
        error: terminal?.type === "turn:error" ? "turn_failed" : json.error,
        message: terminal?.message ?? json.message,
        status: terminal?.type === "turn:error" ? "error" : "idle",
      };
      return { session: ref, response };
    }

    const res = await fetch(path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    const json = (await res.json()) as SessionTurnResponse;
    if (!json.sessionId || !json.continuationToken) {
      throw new Error(json.message ?? json.error ?? `session ${res.status}`);
    }
    const ref = { sessionId: json.sessionId, continuationToken: json.continuationToken };
    if (options.onEvent) {
      await consumeSessionStream(
        `${this.baseUrl}/plasm/v1/session/${ref.sessionId}/stream`,
        options.onEvent,
      );
    }
    return { session: ref, response: json };
  }
}
