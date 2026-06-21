import { randomUUID } from "node:crypto";
import type { ModelMessage } from "ai";

import type { PlasmAgent } from "../runtime/plasm-agent.js";
import type { AgentTurnResult } from "../runtime/plasm-agent.js";

export type SessionEventType =
  | "turn:start"
  | "turn:step"
  | "turn:finish"
  | "turn:error";

export interface SessionEvent {
  id: string;
  type: SessionEventType;
  at: string;
  data: Record<string, unknown>;
}

export interface DevSessionRecord {
  id: string;
  continuationToken: string;
  createdAt: string;
  updatedAt: string;
  status: "idle" | "running" | "error";
  messages: ModelMessage[];
  events: SessionEvent[];
  lastText?: string;
  lastError?: string;
}

type SessionListener = (event: SessionEvent) => void;

export class DevSessionStore {
  private readonly sessions = new Map<string, DevSessionRecord>();
  private readonly listeners = new Map<string, Set<SessionListener>>();

  list(): DevSessionRecord[] {
    return [...this.sessions.values()];
  }

  get(sessionId: string): DevSessionRecord | undefined {
    return this.sessions.get(sessionId);
  }

  create(): DevSessionRecord {
    const id = randomUUID();
    const record: DevSessionRecord = {
      id,
      continuationToken: id,
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
      status: "idle",
      messages: [],
      events: [],
    };
    this.sessions.set(id, record);
    return record;
  }

  subscribe(sessionId: string, listener: SessionListener): () => void {
    let set = this.listeners.get(sessionId);
    if (!set) {
      set = new Set();
      this.listeners.set(sessionId, set);
    }
    set.add(listener);
    return () => {
      set?.delete(listener);
      if (set && set.size === 0) this.listeners.delete(sessionId);
    };
  }

  private pushEvent(
    session: DevSessionRecord,
    type: SessionEventType,
    data: Record<string, unknown>,
  ): SessionEvent {
    const event: SessionEvent = {
      id: randomUUID(),
      type,
      at: new Date().toISOString(),
      data,
    };
    session.events.push(event);
    session.updatedAt = event.at;
    const listeners = this.listeners.get(session.id);
    if (listeners) {
      for (const listener of listeners) listener(event);
    }
    return event;
  }

  validateContinuation(sessionId: string, token?: string): DevSessionRecord | null {
    const session = this.sessions.get(sessionId);
    if (!session) return null;
    if (!token || token !== session.continuationToken) return null;
    return session;
  }

  async runTurn(
    session: DevSessionRecord,
    message: string,
    agent: PlasmAgent,
  ): Promise<AgentTurnResult> {
    if (session.status === "running") {
      throw new Error("session_busy");
    }

    session.status = "running";
    session.messages.push({ role: "user", content: message });
    this.pushEvent(session, "turn:start", { message });

    try {
      const result = await agent.generate(message, {
        messages: [...session.messages],
        onStepFinish: async (step) => {
          const toolsUsed = (step.toolCalls ?? []).map((call) => call.toolName);
          this.pushEvent(session, "turn:step", {
            toolsUsed,
            text: step.text ?? null,
            finishReason: step.finishReason ?? null,
          });
        },
      });

      session.messages.push({ role: "assistant", content: result.text });
      session.lastText = result.text;
      session.status = "idle";
      this.pushEvent(session, "turn:finish", {
        text: result.text,
        steps: result.steps.length,
        usage: result.usage,
      });
      return result;
    } catch (err) {
      session.status = "error";
      session.lastError = String(err);
      this.pushEvent(session, "turn:error", { message: session.lastError });
      throw err;
    }
  }
}

export function formatSseEvent(event: SessionEvent): string {
  const payload = JSON.stringify({
    id: event.id,
    type: event.type,
    at: event.at,
    ...event.data,
  });
  return `event: ${event.type}\ndata: ${payload}\n\n`;
}
