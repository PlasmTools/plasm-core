export interface EveStreamEvent {
  type: string;
  data: Record<string, unknown>;
  meta?: { at?: string };
}

export type EveWorkflowWritable =
  | WritableStream<Uint8Array>
  | {
      getWriter(): WritableStreamDefaultWriter<Uint8Array>;
    };

function stampEvent(event: EveStreamEvent): EveStreamEvent {
  return {
    ...event,
    meta: { at: new Date().toISOString(), ...event.meta },
  };
}

export async function writeEveStreamEvent(
  writable: EveWorkflowWritable,
  event: EveStreamEvent,
): Promise<void> {
  const payload = `${JSON.stringify(stampEvent(event))}\n`;
  const bytes = new TextEncoder().encode(payload);
  if ("getWriter" in writable && typeof writable.getWriter === "function") {
    const writer = writable.getWriter();
    try {
      await writer.write(bytes);
    } finally {
      writer.releaseLock();
    }
    return;
  }
  const stream = writable as WritableStream<Uint8Array>;
  const writer = stream.getWriter();
  try {
    await writer.write(bytes);
  } finally {
    writer.releaseLock();
  }
}

export async function emitSessionStarted(
  writable: EveWorkflowWritable,
  options: { sessionId: string; agentName?: string; modelId?: string; generatorVersion?: string },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "session.started",
    data: {
      sessionId: options.sessionId,
      runtime: {
        agentName: options.agentName,
        modelId: options.modelId,
        eveVersion: options.generatorVersion,
      },
    },
  });
}

export async function emitTurnStarted(
  writable: EveWorkflowWritable,
  options: { turnId: string; sequence: number },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "turn.started",
    data: { turnId: options.turnId, sequence: options.sequence },
  });
}

export async function emitMessageReceived(
  writable: EveWorkflowWritable,
  options: { message: string; turnId: string; sequence: number },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "message.received",
    data: {
      message: options.message,
      turnId: options.turnId,
      sequence: options.sequence,
    },
  });
}

export async function emitStepStarted(
  writable: EveWorkflowWritable,
  options: { turnId: string; stepIndex: number; sequence: number },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "step.started",
    data: {
      turnId: options.turnId,
      stepIndex: options.stepIndex,
      sequence: options.sequence,
    },
  });
}

export async function emitStepCompleted(
  writable: EveWorkflowWritable,
  options: {
    turnId: string;
    stepIndex: number;
    sequence: number;
    finishReason?: string;
    usage?: {
      inputTokens?: number;
      outputTokens?: number;
      cacheReadTokens?: number;
      cacheWriteTokens?: number;
    };
  },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "step.completed",
    data: {
      turnId: options.turnId,
      stepIndex: options.stepIndex,
      sequence: options.sequence,
      finishReason: options.finishReason ?? "stop",
      ...(options.usage ? { usage: options.usage } : {}),
    },
  });
}

export async function emitActionsRequested(
  writable: EveWorkflowWritable,
  options: {
    turnId: string;
    stepIndex: number;
    sequence: number;
    actions: Array<{ callId: string; toolName: string; kind: "tool-call" }>;
  },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "actions.requested",
    data: {
      turnId: options.turnId,
      stepIndex: options.stepIndex,
      sequence: options.sequence,
      actions: options.actions,
    },
  });
}

export async function emitMessageCompleted(
  writable: EveWorkflowWritable,
  options: {
    turnId: string;
    stepIndex: number;
    sequence: number;
    message: string;
    finishReason?: string;
  },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "message.completed",
    data: {
      turnId: options.turnId,
      stepIndex: options.stepIndex,
      sequence: options.sequence,
      message: options.message,
      finishReason: options.finishReason ?? "stop",
    },
  });
}

export async function emitTurnCompleted(
  writable: EveWorkflowWritable,
  options: { turnId: string; sequence: number },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "turn.completed",
    data: { turnId: options.turnId, sequence: options.sequence },
  });
}

export async function emitTurnFailed(
  writable: EveWorkflowWritable,
  options: { turnId: string; sequence: number; code: string; message: string },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "turn.failed",
    data: {
      turnId: options.turnId,
      sequence: options.sequence,
      code: options.code,
      message: options.message,
    },
  });
}

export async function emitSessionWaiting(
  writable: EveWorkflowWritable,
  options: { sessionId: string },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "session.waiting",
    data: { sessionId: options.sessionId, wait: "next-user-message" },
  });
}

export async function emitSessionFailed(
  writable: EveWorkflowWritable,
  options: { sessionId: string; code: string; message: string },
): Promise<void> {
  await writeEveStreamEvent(writable, {
    type: "session.failed",
    data: {
      sessionId: options.sessionId,
      code: options.code,
      message: options.message,
    },
  });
}
