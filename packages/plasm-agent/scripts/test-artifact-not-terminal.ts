#!/usr/bin/env node
import assert from "node:assert/strict";
import { asSchema, jsonSchema, tool } from "ai";
import { MockLanguageModelV3 } from "ai/test";
import type { ModelMessage } from "ai";

import type { AgentRuntime } from "../src/runtime/agent-runtime.js";
import {
  applyArtifactLedger,
  artifactKeysInText,
  gateUnreadArtifactTerminals,
  previewRequiresArtifact,
  runIdFromArtifactRef,
  unreadArtifactTerminalError,
} from "../src/tools/artifact-contract.js";
import { formatPlasmRunMarkdown } from "../src/tools/format.js";
import { createPlasmTools } from "../src/tools/plasm-tools.js";
import { runEveToolLoop } from "../src/telemetry/eve-tool-loop.js";

const RUN_ID = `pr${"ab".repeat(32)}`;
const ARTIFACT_URI = `plasm://execute/ph/s/run/${RUN_ID}`;

assert.equal(runIdFromArtifactRef(RUN_ID), RUN_ID);
assert.equal(runIdFromArtifactRef(ARTIFACT_URI), RUN_ID);
assert.equal(runIdFromArtifactRef("plasm://execute/ph/s/plan/not-a-run"), null);

assert.equal(previewRequiresArtifact("## rows (2)\n```tsv\nid\tname\n1\ta\n```"), false);
assert.equal(
  previewRequiresArtifact("result_delivery\tsnapshot_only\nartifact_uri\t" + ARTIFACT_URI),
  true,
);
assert.equal(
  previewRequiresArtifact("## notes (1)\n```tsv\nbody\n(in artifact)\n```"),
  true,
);
assert.equal(
  previewRequiresArtifact(`resource_link ${ARTIFACT_URI}`),
  true,
);
assert.equal(
  previewRequiresArtifact(`**run_id:** \`${RUN_ID}\` — call **plasm_read_run_artifact** with this id when you need the full snapshot.`),
  false,
  "optional full-snapshot hint must not force a read on inline rows",
);

assert.deepEqual(artifactKeysInText(`artifact_uri\t${ARTIFACT_URI}`).sort(), [ARTIFACT_URI, RUN_ID].sort());

const readSchema = asSchema(
  createPlasmTools({} as AgentRuntime).plasm_read_run_artifact!.inputSchema,
);
assert.ok(readSchema.validate);
assert.equal(
  (await readSchema.validate({ logical_session_ref: "l_ref", run_id: RUN_ID })).success,
  true,
);
assert.equal(
  (await readSchema.validate({ logical_session_ref: "l_ref", artifact_uri: ARTIFACT_URI })).success,
  true,
);
assert.equal(
  (await readSchema.validate({ logical_session_ref: "l_ref" })).success,
  false,
  "schema must reject a missing locator",
);
assert.equal(
  (await readSchema.validate({
    logical_session_ref: "l_ref",
    run_id: RUN_ID,
    artifact_uri: ARTIFACT_URI,
  })).success,
  false,
  "schema must reject both locators",
);

const snapshotOnly = [
  "result_delivery\tsnapshot_only",
  `artifact_uri\t${ARTIFACT_URI}`,
  "```tsv",
  "body",
  "(in artifact)",
  "```",
].join("\n");

const outstanding = new Set<string>();
applyArtifactLedger(outstanding, [
  {
    role: "tool",
    content: [{ type: "tool-result", toolName: "plasm_run", output: { type: "text", value: snapshotOnly } }],
  } as ModelMessage,
]);
assert.ok(outstanding.has(RUN_ID) || outstanding.has(ARTIFACT_URI), `ledger must track unread snapshot, got ${[...outstanding]}`);

applyArtifactLedger(outstanding, [
  {
    role: "assistant",
    content: [
      {
        type: "tool-call",
        toolCallId: "read-1",
        toolName: "plasm_read_run_artifact",
        input: { artifact_uri: ARTIFACT_URI },
      },
    ],
  } as ModelMessage,
  {
    role: "tool",
    content: [
      {
        type: "tool-result",
        toolCallId: "read-1",
        toolName: "plasm_read_run_artifact",
        output: {
          type: "text",
          value: `**run_id:** \`${RUN_ID}\`\nMaterialized under artefact workspace.\n\n\`\`\`json\n{"rows":[{"body":"full"}]}\n\`\`\``,
        },
      },
    ],
  } as ModelMessage,
]);
assert.equal(outstanding.size, 0, "successful plasm_read_run_artifact clears the ledger");

const stillUnread = new Set<string>([RUN_ID]);
applyArtifactLedger(stillUnread, [
  {
    role: "assistant",
    content: [
      {
        type: "tool-call",
        toolCallId: "read-bad",
        toolName: "plasm_read_run_artifact",
        input: { run_id: RUN_ID },
      },
    ],
  } as ModelMessage,
  {
    role: "tool",
    content: [
      {
        type: "tool-error",
        toolCallId: "read-bad",
        toolName: "plasm_read_run_artifact",
        isError: true,
        output: { type: "error-text", value: "plasm_read_run_artifact: unknown run_id" },
      },
    ],
  } as unknown as ModelMessage,
]);
assert.ok(stillUnread.has(RUN_ID), "failed read must not clear the ledger");

const EARLIER_RUN_ID = `pr${"cd".repeat(32)}`;
const EARLIER_URI = `plasm://execute/ph/s/run/${EARLIER_RUN_ID}`;
const mismatchUnread = new Set<string>([EARLIER_RUN_ID, EARLIER_URI]);
applyArtifactLedger(mismatchUnread, [
  {
    role: "assistant",
    content: [
      {
        type: "tool-call",
        toolCallId: "read-later",
        toolName: "plasm_read_run_artifact",
        input: { run_id: RUN_ID },
      },
    ],
  } as ModelMessage,
  {
    role: "tool",
    content: [
      {
        type: "tool-result",
        toolCallId: "read-later",
        toolName: "plasm_read_run_artifact",
        output: {
          type: "text",
          value: `**run_id:** \`${RUN_ID}\`\nMaterialized under artefact workspace.\n\n\`\`\`json\n{"rows":[{"body":"later"}]}\n\`\`\``,
        },
      },
    ],
  } as ModelMessage,
]);
assert.ok(mismatchUnread.has(EARLIER_RUN_ID), "wrong-id read must leave the outstanding snapshot");
assert.ok(mismatchUnread.has(EARLIER_URI), "wrong-id read must leave the outstanding uri");
const mismatchError = unreadArtifactTerminalError(mismatchUnread);
assert.ok(
  mismatchError.includes(EARLIER_RUN_ID),
  `unread error must name the outstanding run_id, got ${mismatchError}`,
);
assert.ok(
  mismatchError.includes(EARLIER_URI),
  `unread error must name the outstanding artifact_uri, got ${mismatchError}`,
);
assert.ok(
  mismatchError.includes(`plasm_read_run_artifact with ${JSON.stringify({ artifact_uri: EARLIER_URI })}`),
  `unread error must include an executable plasm_read_run_artifact call, got ${mismatchError}`,
);
assert.equal(
  mismatchError.includes(RUN_ID),
  false,
  `unread error must not treat the wrong-id read as the remaining snapshot, got ${mismatchError}`,
);
const gated = gateUnreadArtifactTerminals(
  {
    submit_answer: tool({
      inputSchema: jsonSchema({
        type: "object",
        properties: { answer: { type: "string" } },
        required: ["answer"],
      }),
      execute: async () => "Answer submitted.",
    }),
  },
  mismatchUnread,
);
const gatedExecute = gated.submit_answer?.execute as
  | ((input: { answer: string }) => Promise<unknown>)
  | undefined;
assert.ok(gatedExecute, "gated submit_answer must exist");
await assert.rejects(
  () => gatedExecute({ answer: "preview" }),
  (err: unknown) => err instanceof Error && err.message === mismatchError,
);

const SNAPSHOT_A_ID = `pr${"aa".repeat(32)}`;
const SNAPSHOT_A_URI = `plasm://execute/ph/s/run/${SNAPSHOT_A_ID}`;
const SNAPSHOT_B_ID = `pr${"bb".repeat(32)}`;
const SNAPSHOT_B_URI = `plasm://execute/ph/s/run/${SNAPSHOT_B_ID}`;
const sequence = new Set<string>();
applyArtifactLedger(sequence, [
  {
    role: "tool",
    content: [
      {
        type: "tool-result",
        toolName: "plasm_run",
        output: {
          type: "text",
          value: `result_delivery\tsnapshot_only\nartifact_uri\t${SNAPSHOT_A_URI}\n`,
        },
      },
    ],
  } as ModelMessage,
  {
    role: "tool",
    content: [
      {
        type: "tool-result",
        toolName: "plasm_run",
        output: {
          type: "text",
          value: `result_delivery\tsnapshot_only\nartifact_uri\t${SNAPSHOT_B_URI}\nrun_id\t${SNAPSHOT_B_ID}\n`,
        },
      },
    ],
  } as ModelMessage,
]);
assert.ok(sequence.has(SNAPSHOT_A_URI) || sequence.has(SNAPSHOT_A_ID), "A must be outstanding");
assert.ok(sequence.has(SNAPSHOT_B_URI) || sequence.has(SNAPSHOT_B_ID), "B must be outstanding");
applyArtifactLedger(sequence, [
  {
    role: "assistant",
    content: [
      {
        type: "tool-call",
        toolCallId: "read-b",
        toolName: "plasm_read_run_artifact",
        input: { artifact_uri: SNAPSHOT_B_URI },
      },
    ],
  } as ModelMessage,
  {
    role: "tool",
    content: [
      {
        type: "tool-result",
        toolCallId: "read-b",
        toolName: "plasm_read_run_artifact",
        output: {
          type: "text",
          value: `**run_id:** \`${SNAPSHOT_B_ID}\`\n\`\`\`json\n{"rows":[{"ok":true}]}\n\`\`\``,
        },
      },
    ],
  } as ModelMessage,
]);
assert.equal(sequence.has(SNAPSHOT_B_URI), false, "reading B clears the B URI");
assert.equal(sequence.has(SNAPSHOT_B_ID), false, "reading B clears the B run_id twin");
assert.ok(
  sequence.has(SNAPSHOT_A_URI) || sequence.has(SNAPSHOT_A_ID),
  "reading B must leave A outstanding",
);
const sequenceError = unreadArtifactTerminalError(sequence);
assert.ok(sequenceError.includes(SNAPSHOT_A_ID), `remaining must name A, got ${sequenceError}`);
assert.ok(
  sequenceError.includes(`plasm_read_run_artifact with ${JSON.stringify({ artifact_uri: SNAPSHOT_A_URI })}`),
  `agent must recover A from the error args, got ${sequenceError}`,
);
assert.equal(
  sequenceError.includes(SNAPSHOT_B_ID),
  false,
  `read B must not remain in the error, got ${sequenceError}`,
);
const dup = new Set<string>();
applyArtifactLedger(dup, [
  {
    role: "tool",
    content: [
      {
        type: "tool-result",
        toolName: "plasm",
        output: {
          type: "text",
          value: `result_delivery\tsnapshot_only\nartifact_uri\t${SNAPSHOT_A_URI}\nrun_id\t${SNAPSHOT_A_ID}\n`,
        },
      },
    ],
  } as ModelMessage,
]);
const dupCalls = unreadArtifactTerminalError(dup);
const callCount = (dupCalls.match(/plasm_read_run_artifact with /g) ?? []).length;
assert.equal(callCount, 1, `URI and run_id are one obligation, got ${dupCalls}`);

const inline = new Set<string>();
applyArtifactLedger(inline, [
  {
    role: "tool",
    content: [
      {
        type: "tool-result",
        toolName: "plasm_run",
        output: { type: "text", value: formatPlasmRunMarkdown("## rows (1)\n```tsv\nid\n1\n```", true, RUN_ID) },
      },
    ],
  } as ModelMessage,
]);
assert.equal(inline.size, 0, "inline TSV without snapshot_only is already complete");

let submitBeforeRead = 0;
const submitBeforeReadModel = new MockLanguageModelV3({
  doStream: async () => {
    const index = submitBeforeRead++;
    return {
      stream: new ReadableStream({
        start(controller) {
          if (index === 0) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "run-1",
              toolName: "plasm_run",
              input: '{"run_ref":"pc0"}',
            });
          } else if (index === 2) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: `read-${index}`,
              toolName: "plasm_read_run_artifact",
              input: JSON.stringify({ artifact_uri: ARTIFACT_URI }),
            });
          } else {
            controller.enqueue({
              type: "tool-call",
              toolCallId: `ans-${index}`,
              toolName: "submit_answer",
              input: '{"answer":"preview"}',
            });
          }
          controller.enqueue({
            type: "finish",
            finishReason: { unified: "tool-calls", raw: undefined },
            usage: {
              inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
              outputTokens: { total: 2, text: 2, reasoning: 0 },
            },
          });
          controller.close();
        },
      }),
    };
  },
});

const blocked = await runEveToolLoop({
  model: submitBeforeReadModel,
  system: "system",
  tools: {
    plasm_run: tool({
      inputSchema: jsonSchema({ type: "object", properties: {} }),
      execute: async () => snapshotOnly,
    }),
    plasm_read_run_artifact: tool({
      inputSchema: jsonSchema({
        type: "object",
        properties: { artifact_uri: { type: "string" }, run_id: { type: "string" } },
      }),
      execute: async () =>
        `**run_id:** \`${RUN_ID}\`\nMaterialized under artefact workspace.\n\n\`\`\`json\n{"body":"full"}\n\`\`\``,
    }),
    submit_answer: tool({
      inputSchema: jsonSchema({
        type: "object",
        properties: { answer: { type: "string" } },
        required: ["answer"],
      }),
      execute: async () => "Answer submitted.",
    }),
  },
  messages: [{ role: "user", content: "report the value" }],
  maxSteps: 6,
  agentName: "test-artifact-before-submit",
  telemetry: { isEnabled: false },
});

assert.equal(blocked.stopReason, "completed");
assert.ok(submitBeforeRead >= 4, `must reject preview submit, then read, then submit, steps=${submitBeforeRead}`);
const invoked = blocked.messages.flatMap((message) => {
  if (message.role !== "assistant" || !Array.isArray(message.content)) return [];
  return message.content
    .map((part) => (part && typeof part === "object" ? (part as { toolName?: string }).toolName : undefined))
    .filter((name): name is string => typeof name === "string");
});
assert.ok(
  invoked.includes("plasm_read_run_artifact"),
  `plasm_read_run_artifact must run before conclusion, got ${invoked.join(",")}`,
);
assert.ok(
  invoked.lastIndexOf("submit_answer") > invoked.indexOf("plasm_read_run_artifact"),
  "submit_answer after read is the successful terminal",
);

let leftoverCount = 0;
const leftoverModel = new MockLanguageModelV3({
  doStream: async () => {
    leftoverCount += 1;
    return {
      stream: new ReadableStream({
        start(controller) {
          if (leftoverCount === 1) {
            controller.enqueue({
              type: "tool-call",
              toolCallId: "run-left",
              toolName: "plasm_run",
              input: '{"run_ref":"pc0"}',
            });
          } else {
            controller.enqueue({ type: "text-start", id: "text" });
            controller.enqueue({ type: "text-delta", id: "text", delta: "done from preview" });
            controller.enqueue({ type: "text-end", id: "text" });
          }
          controller.enqueue({
            type: "finish",
            finishReason: { unified: leftoverCount === 1 ? "tool-calls" : "stop", raw: undefined },
            usage: {
              inputTokens: { total: 8, noCache: 8, cacheRead: 0, cacheWrite: 0 },
              outputTokens: { total: 2, text: 2, reasoning: 0 },
            },
          });
          controller.close();
        },
      }),
    };
  },
});

const leftover = await runEveToolLoop({
  model: leftoverModel,
  system: "system",
  tools: {
    plasm_run: tool({
      inputSchema: jsonSchema({ type: "object", properties: {} }),
      execute: async () => snapshotOnly,
    }),
  },
  messages: [{ role: "user", content: "read the rows" }],
  maxSteps: 3,
  agentName: "test-unread-artifact-leftover",
  telemetry: { isEnabled: false },
});
assert.equal(
  leftover.stopReason,
  "budget_exhausted",
  "chat stop with unread snapshot is unfinished, not completed",
);
assert.ok(leftoverCount >= 2, "loop must continue past unread snapshot chat-stop");

console.log("test-artifact-not-terminal: ok");
