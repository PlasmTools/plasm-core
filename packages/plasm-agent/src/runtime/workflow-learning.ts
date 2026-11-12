import { z } from "zod";
import { logicalSessionRefSchema } from "./session-contract.js";

/** A bounded model projection; full compiler and symbol evidence stays native. */
export const reviewPacketSchema = z.object({
  version: z.literal(4), execution_id: z.string().regex(/^we[0-9a-f]{64}$/),
  program: z.discriminatedUnion("state", [
    z.object({ state: z.literal("available"), source: z.string().min(1),
      plan: z.discriminatedUnion("state", [z.object({state: z.literal("available"), text: z.string()}).strict(), z.object({state: z.literal("unavailable"), reason: z.literal("plan_not_available")}).strict()]),
      symbols: z.array(z.string()),
      capabilities: z.array(z.string()) }).strict(),
    z.object({ state: z.literal("unavailable"), reason: z.enum(["sensitive_source", "missing_compiler_evidence", "missing_catalogue_evidence", "source_mismatch", "catalogue_source_mismatch"]) }).strict(),
  ]),
}).strict();
export type ReviewPacket = z.infer<typeof reviewPacketSchema>;

const scopeSchema = z.object({ tenant: z.string().min(1), project: z.string().min(1) }).strict();
export const workflowLearningConfigSchema = z.discriminatedUnion("mode", [
  z.object({ mode: z.literal("baseline") }).strict(),
  ...(["collect", "admission", "examples"] as const).map(mode => z.object({
    mode: z.literal(mode), scope: scopeSchema, databaseUrl: z.string().min(1),
    serveExamples: z.boolean().optional(),
  }).strict()),
]);
export type WorkflowLearningConfig = z.infer<typeof workflowLearningConfigSchema>;

/** Explicit trusted-host delegation; never accepted as a model tool argument. */
export const trainingHostConfigSchema = z.object({
  subject: z.string().min(1), credential: z.string().min(1),
  idempotency_key: z.string().uuid().optional(),
  grant: z.object({
    grantee: z.string().min(1), credential: z.string().min(1),
    target: z.object({ scope: z.literal("project") }).strict(),
    actions: z.array(z.enum(["capture", "review", "publish_automatically", "release", "attest_imported_review"])).min(1),
    expires_at: z.string().datetime(),
    publication_policy: z.discriminatedUnion("policy", [
      z.object({ policy: z.literal("human_reviewed") }).strict(),
      z.object({ policy: z.literal("delegated_automatic") }).strict(),
    ]),
  }).strict().optional(),
  requestForNewSessions: z.boolean(),
  approvalBaseUrl: z.string().url().refine(value => {
    const url = new URL(value);
    return ["http:", "https:"].includes(url.protocol) && !url.username && !url.password;
  }, "Approval URL must be HTTP(S) without credentials").optional(),
}).strict()
  .refine(config => !config.grant || config.idempotency_key !== undefined, "A training grant requires an idempotency key")
  .refine(config => !config.grant || (config.grant.grantee === config.subject && config.grant.credential === config.credential), "Training grant must belong to the configured caller");
export type TrainingHostConfig = z.infer<typeof trainingHostConfigSchema>;
export const trainingStatusSchema = z.object({
  revision: z.number().int().nonnegative(),
  training_state: z.object({ state: z.enum(["ordinary", "awaiting_authorization", "active", "stopped"]) }).passthrough(),
}).passthrough();
export type TrainingStatus = z.infer<typeof trainingStatusSchema>;
export type TrainingCandidateReference = { review_ref: string; candidate_hash: string };
export type TrainingCandidateAction =
  | { action: "list"; limit: number; after?: TrainingCandidateReference }
  | { action: "read"; reference: TrainingCandidateReference }
  | { action: "request_status"; request_ref: string }
  | { action: "request_decision"; expected_revision: number; idempotency_key: string; selection: Array<TrainingCandidateReference & {
      guidance_digest: string; action: { decision: "approve" } | { decision: "reject" | "request_revision"; reason: string };
    }> }
  | { action: "request_release"; expected_revision: number; idempotency_key: string; review_refs: string[] }
  | { action: "request_execution_release"; expected_revision: number; idempotency_key: string; execution_ids: string[] };

export const workflowReviewInputSchema = z.object({
  logical_session_ref: logicalSessionRefSchema,
  expected_revision: z.number().int().nonnegative(),
  idempotency_key: z.string().uuid(),
  subtasks: z.array(z.object({
    intent: z.string().min(1), observed_outcome: z.string().min(1),
    execution_ids: z.array(z.string().regex(/^we[0-9a-f]{64}$/)).min(1),
  }).strict()).min(1).max(16),
}).strict();
export type WorkflowReviewInput = z.infer<typeof workflowReviewInputSchema>;
export const completedWorkflowExecutionsSchema = z.object({
  intent: z.string(), executions: z.array(z.object({ execution_id: z.string().regex(/^we[0-9a-f]{64}$/) }).passthrough()),
});
export type CompletedWorkflowExecutions = z.infer<typeof completedWorkflowExecutionsSchema>;
export interface CompletedExecutionIndex {
  sessions: Array<CompletedWorkflowExecutions & { logical_session_ref: z.infer<typeof logicalSessionRefSchema> }>;
}
export const workflowReviewReceiptSchema = z.object({ review_ref: z.string().min(1) }).passthrough();
export type WorkflowReviewReceipt = z.infer<typeof workflowReviewReceiptSchema>;
export const servedWorkflowExampleSchema = z.object({
  example_id: z.string().uuid(), source: z.string().min(1), teaching_delta: z.string(),
  domain_revision: z.number().int().nonnegative(), delta_refs: z.array(z.string().min(1)),
  outputs: z.array(z.string()),
}).strict();
export type ServedWorkflowExample = z.infer<typeof servedWorkflowExampleSchema>;

/** Semantic program inspection and explicit sealing after effect admission closes. */
export interface WorkflowReviewSession {
  completedExecutions(): Promise<CompletedExecutionIndex>;
  trainingStatus(logicalSessionRef: string): Promise<TrainingStatus>;
  readProgram(logicalSessionRef: string, executionId: string): Promise<ReviewPacket>;
  submitReview(input: WorkflowReviewInput): Promise<WorkflowReviewReceipt>;
}

/** Native capability; receipts and eligibility remain owned by the Rust engine. */
export interface WorkflowLearningEngine {
  configure(config: WorkflowLearningConfig): Promise<void>;
  configureTraining(host: Pick<TrainingHostConfig, "subject" | "credential" | "grant" | "idempotency_key">): Promise<void>;
  sealTrainingEvaluation(logicalSessionId: string): Promise<void>;
  trainingStatus(logicalSessionId: string): Promise<TrainingStatus>;
  trainingCommand(logicalSessionId: string, command: { idempotency_key: string; expected_revision: number; command: { action: "request" | "stop" | "resume" } }): Promise<TrainingStatus>;
  trainingCandidates(logicalSessionId: string, command: TrainingCandidateAction): Promise<unknown>;
  drainSession(logicalSessionId: string): Promise<void>;
  releaseSession(logicalSessionId: string): Promise<void>;
  completedExecutions(logicalSessionId: string): Promise<CompletedWorkflowExecutions>;
  readProgram(logicalSessionId: string, executionId: string): Promise<ReviewPacket>;
  review(logicalSessionId: string, subtasks: WorkflowReviewInput["subtasks"], expectedRevision: number, idempotencyKey: string): Promise<WorkflowReviewReceipt>;
  fetchExample(logicalSessionId: string, revisionRef: string): Promise<ServedWorkflowExample>;
}
