import { z } from "zod";
import { logicalSessionRefSchema } from "./session-contract.js";

const scopeSchema = z.object({ tenant: z.string().min(1), project: z.string().min(1) }).strict();
export const workflowLearningConfigSchema = z.discriminatedUnion("mode", [
  z.object({ mode: z.literal("baseline") }).strict(),
  ...(["collect", "admission", "examples"] as const).map(mode => z.object({
    mode: z.literal(mode), scope: scopeSchema, databaseUrl: z.string().min(1),
  }).strict()),
]);
export type WorkflowLearningConfig = z.infer<typeof workflowLearningConfigSchema>;

export const workflowReviewInputSchema = z.object({
  logical_session_ref: logicalSessionRefSchema,
  subtasks: z.array(z.object({
    intent: z.string().min(1), observed_outcome: z.string().min(1),
    run_ids: z.array(z.string().min(1)).min(1),
  }).strict()).min(1).max(16),
}).strict();
export type WorkflowReviewInput = z.infer<typeof workflowReviewInputSchema>;
export const completedWorkflowRunsSchema = z.object({
  intent: z.string(), runs: z.array(z.object({ run_id: z.string().min(1) }).passthrough()),
});
export type CompletedWorkflowRuns = z.infer<typeof completedWorkflowRunsSchema>;
export interface CompletedRunIndex {
  sessions: Array<CompletedWorkflowRuns & { logical_session_ref: z.infer<typeof logicalSessionRefSchema> }>;
}
export const workflowReviewReceiptSchema = z.object({ review_ref: z.string().min(1) }).passthrough();
export type WorkflowReviewReceipt = z.infer<typeof workflowReviewReceiptSchema>;
export const servedWorkflowExampleSchema = z.object({
  example_id: z.string().uuid(), source: z.string().min(1), teaching_delta: z.string(),
  domain_revision: z.number().int().nonnegative(), delta_refs: z.array(z.string().min(1)),
  outputs: z.array(z.unknown()),
  catalog_fingerprints: z.record(z.string()),
}).strict();
export type ServedWorkflowExample = z.infer<typeof servedWorkflowExampleSchema>;

/** Read-only evidence access and explicit sealing after effect admission closes. */
export interface WorkflowReviewSession {
  completedRuns(): Promise<CompletedRunIndex>;
  readEvidence(logicalSessionRef: string, runId: string): Promise<unknown>;
  submitReview(input: WorkflowReviewInput): Promise<WorkflowReviewReceipt>;
}

/** Native capability; receipts and eligibility remain owned by the Rust engine. */
export interface WorkflowLearningEngine {
  configure(config: WorkflowLearningConfig): Promise<void>;
  drain(): Promise<void>;
  completedRuns(logicalSessionId: string): Promise<CompletedWorkflowRuns>;
  readEvidence(logicalSessionId: string, runId: string): Promise<unknown>;
  review(logicalSessionId: string, subtasks: WorkflowReviewInput["subtasks"]): Promise<WorkflowReviewReceipt>;
  fetchExample(logicalSessionId: string, revisionRef: string): Promise<ServedWorkflowExample>;
}
