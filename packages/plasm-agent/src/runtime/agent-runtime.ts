import { DrainingLifecycle } from "./lifecycle.js";
import { AgentExecutionFailure, executionFailureSchema, type ExecutionFailure } from "./execution-failure.js";
import { routingExplanationLines, routingRecoveryMarkdown } from "../engine/routing.js";
import path from "node:path";
import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";


import type { LoadedCatalog } from "../catalog/loader.js";
import { FilesystemCatalogLoader } from "../catalog/loader.js";
import {
  createEngine,
  type HostTransportFn,
  type PlasmEngine,
} from "../engine/napi-binding.js";
import { createDefaultHostTransport } from "../engine/host-transport.js";
import { formatLogicalSessionWireRef } from "../runtime/logical-session.js";
import { logicalSessionRefSchema, runReferenceSchema, workflowIntentSchema, deriveIntent, type IntentProvenance } from "./session-contract.js";
import { SessionManager, type AgentSessionState } from "../session-state.js";
import {
  formatPlasmContextMarkdown,
  formatPlasmDryRunMarkdown,
  formatPlasmRunMarkdown,
} from "../tools/format.js";
import { LocalArchiveStore } from "../archive/index.js";
import { createArchiveStore } from "../archive/resolve-backend.js";
import type { ProdArchiveStore } from "../archive/prod-archive-store.js";
import { z } from "zod";
import { writeWorkspaceFile } from "../tools/workspace-files.js";
import { activeTraceId, plasmSpans } from "../telemetry/plasm-spans.js";
import { PlasmSpanAttributes } from "../instrumentation.js";
import type { AuthoringContext } from "../authoring/context.js";
import type { HookRunner } from "../authoring/hook-runner.js";
import type { AgentWorkflowWorldDefinition } from "../define-agent.js";
import { createAgentStateStore } from "../state/define-state.js";
import { DiscoveryQueue } from "./discovery-queue.js";
import { formatRecordedOutputs, runOutputArtifactSchema } from "../tools/recorded-outputs.js";
import { checkObligationEvidence, sameObligationPredicate, type ObligationVerification, type EvidenceCheck } from "../tools/obligation-evidence.js";
import { workflowLearningConfigSchema, workflowReviewInputSchema, type WorkflowLearningConfig, type WorkflowReviewSession } from "./workflow-learning.js";

export type AgentArchiveStore = LocalArchiveStore | ProdArchiveStore;

export interface AgentRuntimeConfig {
  workflowLearning?: WorkflowLearningConfig;
  agentRoot: string;
  /** Optional first intent supplied by the embedding workflow. */
  initialIntent?: string;
  tenantScope?: string;
  engine?: PlasmEngine;
  /** Outbound HTTP for live `plasm_run`. Defaults to fetch + env bearer + Connect. Set `null` to validate-only. */
  hostTransport?: HostTransportFn | null;
  /** When false, skip local archive writes. Default true. */
  archiveEnabled?: boolean;
  archive?: AgentArchiveStore | null;
  /** Workflow/state world — selects fs vs KV vs Postgres session mirror. */
  stateWorld?: AgentWorkflowWorldDefinition;
  hookRunner?: HookRunner;
  getAuthoringContext?: () => AuthoringContext;
  /**
   * Directory for materializing run snapshots for harness TS transforms.
   * Defaults to `PLASM_RUN_ARTIFACTS_DIR` or `<agentRoot>/.plasm/artefacts`.
   */
  artefactWorkspaceRoot?: string;
}

export interface DiscoverInput {
  intent: string;
}

export interface PlasmContextInput {
  intent: string;
  sessionMode?: "new" | "extend";
  logicalSessionRef?: string;
}

export interface PlasmPlanInput {
  logicalSessionRef: string;
  program: string;
}

export interface PlasmRunInput {
  logicalSessionRef: string;
  runRef: string;
}

function seedKey(seed: { api: string; entity: string }): string {
  return `${seed.api}:${seed.entity}`;
}

function mergeSeeds(
  existing: Array<{ api: string; entity: string }>,
  incoming: Array<{ api: string; entity: string }>,
): Array<{ api: string; entity: string }> {
  const seen = new Set(existing.map(seedKey));
  const out = [...existing];
  for (const seed of incoming) {
    const key = seedKey(seed);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(seed);
  }
  return out;
}

function planArchiveEnabled(): boolean {
  const flag = process.env.PLASM_WRITE_PLAN_ARCHIVE?.trim();
  if (flag === "0" || flag === "false") return false;
  return true;
}

interface RunPlasmMeta {
  steps?: Array<{ request_fingerprints?: string[] }>;
  request_fingerprints?: string[];
}

function parseRunMeta(metaJson?: string): RunPlasmMeta | undefined {
  if (!metaJson?.trim()) return undefined;
  try {
    const parsed = JSON.parse(metaJson) as { plasm?: RunPlasmMeta };
    return parsed.plasm;
  } catch {
    return undefined;
  }
}

function collectRequestFingerprints(meta?: RunPlasmMeta): string[] {
  if (!meta) return [];
  const fromSteps = (meta.steps ?? []).flatMap((step) => step.request_fingerprints ?? []);
  const direct = meta.request_fingerprints ?? [];
  return [...new Set([...fromSteps, ...direct])];
}

function parseRowsJson(rowsJson?: string): unknown[] | undefined {
  if (!rowsJson?.trim()) return undefined;
  try {
    const parsed = JSON.parse(rowsJson) as unknown;
    return Array.isArray(parsed) ? parsed : [parsed];
  } catch {
    return undefined;
  }
}

export class AgentRuntime {
  private readonly observingRuns = new Map<string, Set<string>>();
  private readonly observationWrites = new Map<string, Promise<void>>();

  private async persistObservation(session: AgentSessionState): Promise<void> {
    const prior = this.observationWrites.get(session.logicalSessionRef) ?? Promise.resolve();
    const write = prior.then(() => this.sessionManager.update(session));
    this.observationWrites.set(session.logicalSessionRef, write);
    try { await write; }
    finally {
      if (this.observationWrites.get(session.logicalSessionRef) === write) this.observationWrites.delete(session.logicalSessionRef);
    }
  }

  private async beginObservation(session: AgentSessionState): Promise<string> {
    const ref = randomUUID();
    const active = this.observingRuns.get(session.logicalSessionRef) ?? new Set<string>();
    active.add(ref);
    this.observingRuns.set(session.logicalSessionRef, active);
    session.observationRevision = ref;
    await this.persistObservation(session);
    return ref;
  }

  private async endObservation(sessionRef: string, ref: string): Promise<string | undefined> {
    const session = await this.requireSessionByRef(sessionRef);
    const active = this.observingRuns.get(sessionRef);
    const exclusive = active?.size === 1 && active.has(ref) && session.observationRevision === ref;
    active?.delete(ref);
    if (!active?.size) this.observingRuns.delete(sessionRef);
    // Every attempted execution invalidates older evidence, including partial failures.
    const completedRevision = randomUUID();
    session.observationRevision = completedRevision;
    await this.persistObservation(session);
    return exclusive ? completedRevision : undefined;
  }

  async sameObligationPredicate(previous: ObligationVerification, next: ObligationVerification): Promise<boolean> {
    if (!await this.evidenceSession(previous.logical_session_ref)) return false;
    if (!this.archive || previous.logical_session_ref !== next.logical_session_ref) return false;
    const [before, after] = await Promise.all([
      this.archive.getPlan(previous.plan_commit_ref, previous.logical_session_ref),
      this.archive.getPlan(next.plan_commit_ref, next.logical_session_ref),
    ]);
    return sameObligationPredicate(previous, next, before, after);
  }

  async checkObligation(verification: ObligationVerification, runId?: string): Promise<EvidenceCheck> {
    if (!await this.evidenceSession(verification.logical_session_ref)) return { ok: false, code: "unavailable" };
    if (!this.archive) return { ok: false, code: "unavailable" };
    const plan = await this.archive.getPlan(verification.plan_commit_ref, verification.logical_session_ref);
    const run = runId === undefined ? undefined : await this.archive.getRun(runId, verification.logical_session_ref);
    if (run && run.run_id !== runId) return { ok: false, code: "wrong_plan" };
    let stillCurrent: (() => boolean) | undefined;
    if (run) {
      const current = await this.evidenceSession(verification.logical_session_ref);
      if (!current) return { ok: false, code: "unavailable" };
      const revision = run.observation_revision;
      stillCurrent = () => revision !== undefined && revision === current.observationRevision &&
        !this.observingRuns.has(verification.logical_session_ref);
      if (!stillCurrent()) return { ok: false, code: "stale_observation" };
    }
    const checked = checkObligationEvidence(verification, plan, run);
    return checked.ok && stillCurrent ? { ok: true, current: stillCurrent } : checked;
  }
  private readonly lifecycle = new DrainingLifecycle();
  private readonly reviewLifecycle = new DrainingLifecycle();
  private readonly learningConfig: WorkflowLearningConfig;
  private learningConfigured?: Promise<void>;
  private reviewSession?: Promise<WorkflowReviewSession>;
  private readonly learningSessions = new Map<string, string>();

  private configureLearning(): Promise<void> {
    this.learningConfigured ??= this.engine.learning
      ? this.engine.learning.configure(this.learningConfig)
      : this.learningConfig.mode === "baseline" ? Promise.resolve()
        : Promise.reject(new Error("Native engine does not support workflow learning"));
    return this.learningConfigured;
  }

  beginWorkflowReview(): Promise<WorkflowReviewSession> {
    if (this.learningConfig.mode !== "collect") return Promise.reject(new Error("Workflow review requires collect mode"));
    this.reviewSession ??= this.enterWorkflowReview();
    return this.reviewSession;
  }

  private async enterWorkflowReview(): Promise<WorkflowReviewSession> {
    await this.lifecycle.drain();
    await this.configureLearning();
    const learning = this.engine.learning;
    if (!learning) throw new Error("Native engine does not support workflow learning");
    await learning.drain();
    const sessionId = (ref: string): string => {
      const id = this.learningSessions.get(logicalSessionRefSchema.parse(ref));
      if (!id) throw new Error("Review session does not belong to this runtime");
      return id;
    };
    return Object.freeze({
      completedRuns: () => this.reviewLifecycle.run(async () => ({ sessions: await Promise.all(
        [...this.learningSessions].map(async ([ref, id]) => ({
          ...await learning.completedRuns(id), logical_session_ref: logicalSessionRefSchema.parse(ref),
        })),
      ) })),
      readEvidence: (ref: string, run: string) => this.reviewLifecycle.run(() => learning.readEvidence(sessionId(ref), run)),
      submitReview: (input: Parameters<WorkflowReviewSession["submitReview"]>[0]) => this.reviewLifecycle.run(() => {
        const checked = workflowReviewInputSchema.parse(input);
        return learning.review(sessionId(checked.logical_session_ref), checked.subtasks);
      }),
    });
  }

  workflowExampleServingEnabled(): boolean { return this.learningConfig.mode === "examples"; }

  plasmExample(input: { logicalSessionRef: string; revisionRef: string }): Promise<string> {
    const { logicalSessionRef, revisionRef } = input;
    return this.lifecycle.run(async () => {
      if (!this.workflowExampleServingEnabled()) throw new Error("Example fetch is disabled for this runtime");
      const session = await this.requireSessionByRef(logicalSessionRef);
      const learning = this.engine.learning;
      if (!learning) throw new Error("Native engine does not support workflow learning");
      const example = await learning.fetchExample(session.logicalSessionId, revisionRef);
      const exposed = example.delta_refs.map(ref => {
        const separator = ref.indexOf(":");
        if (separator < 1) throw new Error("Invalid example teaching entity reference");
        return { api: ref.slice(0, separator), entity: ref.slice(separator + 1) };
      });
      session.seeds = mergeSeeds(session.seeds, exposed);
      if (example.teaching_delta.trim()) {
        session.teachingPrompt = [session.teachingPrompt.trim(), example.teaching_delta.trim()].filter(Boolean).join("\n\n");
        session.waves.push({ entryId: exposed[0]?.api ?? session.seeds[0]?.api ?? "unknown",
          entities: exposed.map(entry => entry.entity), prompt: example.teaching_delta, at: new Date().toISOString() });
      }
      await this.sessionManager.update(session);
      this.workflowSession = session;
      return JSON.stringify(example);
    });
  }

  /** Stop new admissions, drain accepted work and close the owned engine. */
  close(): Promise<void> {
    return this.lifecycle.close(async () => {
      await this.reviewSession?.catch(() => undefined);
      await this.reviewLifecycle.close(() => this.engine.close());
    });
  }

  private readonly engineInstanceId = randomUUID();
  readonly engine: PlasmEngine;
  readonly sessionManager: SessionManager;
  readonly archive: AgentArchiveStore | null;
  readonly hostTransport: HostTransportFn | null;
  readonly artefactWorkspaceRoot: string;
  private readonly agentRoot: string;
  private readonly initialProvenance?: IntentProvenance;
  private readonly archiveEnabled: boolean;
  private readonly hookRunner?: HookRunner;
  private readonly getAuthoringContext?: () => AuthoringContext;
  private loadedCatalogs: LoadedCatalog[] = [];
  /** Most recently used live session; explicit new calls create distinct workflows. */
  private workflowSession: AgentSessionState | null = null;


  /** True after the first successful `plasm_context` mint on this runtime. */
  hasOpenWorkflow(): boolean {
    return this.workflowSession != null;
  }

  /** Successful `plasm_run` write-node count this process (fused 0w reads stay 0). */
  private committedWriteOps = 0;


  /** Live write ops committed via `plasm_run` on this runtime. */
  committedLiveWriteOps(): number {
    return this.committedWriteOps;
  }

  /** Cached Python language reference and accumulated domain declaration waves (empty before mint). */
  workflowTeachingPrompt(): string | undefined {
    const prompt = this.workflowSession?.teachingPrompt?.trim();
    return prompt || undefined;
  }

  constructor(config: AgentRuntimeConfig) {
    this.learningConfig = workflowLearningConfigSchema.parse(config.workflowLearning ?? { mode: "baseline" });
    this.agentRoot = path.resolve(config.agentRoot);
    this.initialProvenance = config.initialIntent === undefined ? undefined : deriveIntent(undefined, workflowIntentSchema.parse(config.initialIntent));
    this.engine = config.engine ?? createEngine();
    this.hostTransport =
      config.hostTransport === null
        ? null
        : (config.hostTransport ?? createDefaultHostTransport({ useConnect: true }));
    const tenantScope = config.tenantScope ?? "local";
    const stateStore = createAgentStateStore({
      agentRoot: this.agentRoot,
      tenantScope,
      world: config.stateWorld,
    });
    this.sessionManager = new SessionManager(stateStore, tenantScope);
    this.archiveEnabled = config.archiveEnabled ?? true;
    this.hookRunner = config.hookRunner;
    this.getAuthoringContext = config.getAuthoringContext;
    const envArtefacts = process.env.PLASM_RUN_ARTIFACTS_DIR?.trim();
    this.artefactWorkspaceRoot = path.resolve(
      config.artefactWorkspaceRoot ??
      envArtefacts ??
      path.join(this.agentRoot, ".plasm", "artefacts"),
    );
    if (config.archive === null) {
      this.archive = null;
    } else if (config.archive) {
      this.archive = config.archive;
    } else if (this.archiveEnabled) {
      this.archive = createArchiveStore(this.agentRoot, { world: config.stateWorld });
    } else {
      this.archive = null;
    }
  }

  bootstrap(): Promise<LoadedCatalog[]> {
    return this.lifecycle.run(() => this.bootstrapAdmitted());
  }

  private async bootstrapAdmitted(): Promise<LoadedCatalog[]> {
    await this.configureLearning();
    if (this.archive) {
      await this.archive.bootstrap();
    }
    await mkdir(this.artefactWorkspaceRoot, { recursive: true });
    const loader = new FilesystemCatalogLoader();
    const catalogs = await loader.discover(this.agentRoot);
    for (const catalog of catalogs) {
      await this.engine.loadCatalog(catalog);
    }
    const bindingPath = process.env.PLASM_DISCOVERY_BINDINGS_PATH?.trim()
      ?? path.join(this.agentRoot, "catalogs", "deployment-bindings.json");
    const deploymentId = process.env.PLASM_DISCOVERY_DEPLOYMENT?.trim()
      || createHash("sha256").update(`${this.agentRoot}\0${this.sessionManager.tenant()}`).digest("hex");
    await this.engine.activateDiscovery(deploymentId, await readFile(bindingPath, "utf8"));
    this.loadedCatalogs = catalogs;
    return catalogs;
  }

  listCatalogs(): LoadedCatalog[] {
    return [...this.loadedCatalogs];
  }

  private readonly discoveryQueue = new DiscoveryQueue();

  plasmContext(input: PlasmContextInput): Promise<string> {
    return this.lifecycle.run(() => this.plasmContextAdmitted(input));
  }

  private async plasmContextAdmitted(input: PlasmContextInput): Promise<string> {
    // Snapshot inputs before waiting: callers must not mutate a queued request.
    const request: PlasmContextInput = {
      ...input,
      intent: workflowIntentSchema.parse(input.intent),
    };
    if (request.sessionMode !== "extend") return this.plasmContextTurn(request);
    const ref = this.parseSessionRef(request.logicalSessionRef);
    return this.discoveryQueue.run(
      ref,
      request.intent,
      () => this.plasmContextTurn(request),
    );
  }

  private async plasmContextTurn(input: PlasmContextInput): Promise<string> {
    await this.configureLearning();
    const intent = workflowIntentSchema.parse(input.intent);
    const mode = input.sessionMode ?? "new";
    if (!intent) throw new Error("plasm_context requires intent");
    return plasmSpans.toolContext({ intent }, async (span) => {
      const started = Date.now();
      const existing = mode === "extend"
        ? await this.requireSessionByRef(input.logicalSessionRef ?? "")
        : undefined;
      if (mode === "new" && input.logicalSessionRef) {
        throw this.sessionInputFailure("invalid_context_input", "logical_session_ref belongs on session_mode extend");
      }
      const provenance = deriveIntent(existing?.intentProvenance ?? this.initialProvenance, intent);
      const packet = await this.engine.routeIntent(
        provenance,
        existing?.logicalSessionId,
      );
      const { routing, teaching } = packet;
      if (packet.workflow_examples?.length && !this.workflowExampleServingEnabled()) {
        throw new Error("Native discovery disclosed examples in a mode that forbids them");
      }
      const examplesMarkdown = packet.workflow_examples?.length
        ? `Optional workflow examples (fetch with plasm_example):\n${JSON.stringify(packet.workflow_examples)}`
        : undefined;
      if (JSON.stringify(routing.intent_provenance) !== JSON.stringify(provenance)) throw new Error("Routing changed intent provenance");
      if (existing && (existing.logicalSessionId !== routing.pin_id
        || existing.registryGeneration !== routing.retrieval.generation)) {
        throw new Error("Routing changed a logical session's pinned registry generation");
      }
      const wireRef = logicalSessionRefSchema.parse(formatLogicalSessionWireRef(Buffer.from(routing.pin_id.replace(/-/g, ""), "hex")));
      const session = existing ?? await this.sessionManager.getOrCreate({
        intent: provenance.nodes[0]!.intent, intentProvenance: provenance, logicalSessionRef: wireRef, logicalSessionId: routing.pin_id,
      });
      session.intentProvenance = provenance;
      session.engineInstanceId = this.engineInstanceId;
      session.registryGeneration = routing.retrieval.generation;
      await this.sessionManager.update(session);
      this.workflowSession = session;
      this.learningSessions.set(session.logicalSessionRef, session.logicalSessionId);
      const recoveryMarkdown = routingRecoveryMarkdown(routing);
      if (!routing.closure) {
        await this.recordToolTrace("tool", "plasm_context", started, {
          intent, session_mode: mode, routing: JSON.stringify(routing),
          logical_session_ref: session.logicalSessionRef,
        });
        return [
          routing.intent_analysis,
          recoveryMarkdown ?? "**plasm_context:** no relevant capabilities selected",
          `**logical_session_ref:** \`${session.logicalSessionRef}\``,
          ...(recoveryMarkdown ? [] : routingExplanationLines(routing.matching)),
          examplesMarkdown,
        ].filter(Boolean).join("\n\n");
      }
      if (!teaching || (!teaching.prompt.trim() && (!existing || teaching.declarations_changed))) {
        throw new Error("Routing is missing its prerequisite closure or canonical teaching");
      }
      // Keep exact capability and prerequisite distinctions as returned by Rust.
      session.routingClosures = [...(session.routingClosures ?? []), routing.closure];
      const exposed = teaching.delta_refs.map((ref) => {
        const separator = ref.indexOf(":");
        if (separator < 1) throw new Error("Invalid native teaching entity reference");
        return { api: ref.slice(0, separator), entity: ref.slice(separator + 1) };
      });
      session.seeds = mergeSeeds(session.seeds, exposed);
      session.teachingPrompt = [session.teachingPrompt.trim(), teaching.prompt.trim()].filter(Boolean).join("\n\n");
      if (teaching.prompt.trim()) {
        session.waves.push({
          entryId: exposed[0]?.api ?? "unknown", entities: exposed.map((entry) => entry.entity),
          prompt: teaching.prompt, at: new Date().toISOString(),
        });
      }
      await this.sessionManager.update(session);
      this.workflowSession = session;
      span.setAttribute("plasm.logical_session_ref", session.logicalSessionRef);
      await this.recordToolTrace("tool", "plasm_context", started, {
        intent, logical_session_ref: session.logicalSessionRef, session_mode: mode,
        registry_generation: session.registryGeneration, routing: JSON.stringify(routing),
        trace_id: activeTraceId() ?? span.spanContext().traceId,
      });
      const teachingMarkdown = formatPlasmContextMarkdown(
        session.logicalSessionRef, teaching.prompt,
        !existing ? "initial" : teaching.declarations_changed ? "extended" : "unchanged",
      );
      return [routing.intent_analysis, teachingMarkdown, recoveryMarkdown, examplesMarkdown].filter(Boolean).join("\n\n");
    });
  }

  plasm(input: PlasmPlanInput): Promise<string> {
    return this.lifecycle.run(() => this.plasmAdmitted(input));
  }

  private async plasmAdmitted(input: PlasmPlanInput): Promise<string> {
    const session = await this.requireSessionByRef(input.logicalSessionRef);
    const entryId = session.seeds[0]?.api;
    const catalogCgsHash = entryId ? this.catalogHashForEntry(entryId) : undefined;

    return plasmSpans.dryRun(
      {
        intent: session.intent,
        logicalSessionRef: session.logicalSessionRef,
        sessionId: session.logicalSessionId,
        entryId,
        catalogCgsHash,
      },
      async (span) => {
        const started = Date.now();
        const dry = await this.engine.dryRun(input.program, session.logicalSessionId);
        if (dry.failureJson) {
          const failure = executionFailureSchema.parse(JSON.parse(dry.failureJson));
          throw new AgentExecutionFailure(failure, dry.summary);
        }
        if (!Number.isSafeInteger(dry.writeCount) || dry.writeCount < 0) {
          throw new Error("Engine did not provide a valid typed write count");
        }
        session.planCommits.push({
          ref: dry.planCommitRef,
          program: input.program,
          at: new Date().toISOString(),
          writeCount: dry.writeCount,
        });
        await this.sessionManager.update(session);

        span.setAttribute(PlasmSpanAttributes.RUN_REF, dry.planCommitRef);
        if (catalogCgsHash) {
          span.setAttribute("plasm.catalog_cgs_hash", catalogCgsHash);
        }

        if (this.archive && planArchiveEnabled() && catalogCgsHash) {
          await this.archive.writePlanArchive({
            plan_commit_ref: dry.planCommitRef,
            program: input.program,
            catalog_cgs_hash: catalogCgsHash,
            entry_id: entryId,
            logical_session_ref: session.logicalSessionRef,
            intent: session.intent,
            comp_json: dry.compJson,
            archived_at: new Date().toISOString(),
          });
        }

        await this.recordToolTrace("plasm", "plasm.dry_run", started, {
          intent: session.intent,
          logical_session_ref: session.logicalSessionRef,
          plan_commit_ref: dry.planCommitRef,
          catalog_cgs_hash: catalogCgsHash,
          trace_id: activeTraceId() ?? span.spanContext().traceId,
        });

        await this.emitHook("plan:commit", {
          intent: session.intent,
          planCommitRef: dry.planCommitRef,
          runRef: dry.planCommitRef,
          program: input.program,
          logicalSessionRef: session.logicalSessionRef,
        });

        if (dry.fusedCleanRead) {
          return this.plasmRunAdmitted({
            logicalSessionRef: input.logicalSessionRef,
            runRef: dry.planCommitRef,
          });
        }

        return formatPlasmDryRunMarkdown(dry.summary, dry.planCommitRef);
      },
    );
  }

  plasmRun(input: PlasmRunInput): Promise<string> {
    return this.lifecycle.run(() => this.plasmRunAdmitted(input));
  }

  private async plasmRunAdmitted(input: PlasmRunInput): Promise<string> {
    const session = await this.requireSessionByRef(input.logicalSessionRef);
    if (!runReferenceSchema.safeParse(input.runRef).success) {
      throw new AgentExecutionFailure({
        cause: "program", recovery: "repair_program", code: "invalid_run_reference",
        diagnostic: "Copy the reviewed commit reference from plasm or a continuation handle from plasm_run verbatim. Run artifact IDs identify observations, not executable plans.",
        node: null, occurrence_path: [], catalog_digest: null, effects: [], dispatches: [], effects_unresolved: false,
      }, "Execution stopped: copy the reviewed run reference verbatim. Revise the program and plan again.");
    }
    const entryId = session.seeds[0]?.api;
    const catalogCgsHash = entryId ? this.catalogHashForEntry(entryId) : undefined;

    return plasmSpans.liveRun(
      {
        intent: session.intent,
        logicalSessionRef: session.logicalSessionRef,
        sessionId: session.logicalSessionId,
        runRef: input.runRef,
        entryId,
        catalogCgsHash,
      },
      async (span) => {
        const started = Date.now();
        if (this.hostTransport && !this.engine.runPlanLive) {
          throw new Error("Engine does not support reviewed live execution");
        }
        const observation = await this.beginObservation(session);
        let observationEnded = false;
        try {
          const result = this.hostTransport
            ? await this.engine.runPlanLive!(input.runRef, this.hostTransport, session.logicalSessionId)
            : await this.engine.runPlan(input.runRef, session.logicalSessionId);
          if (result.failureJson) {
            const failure = executionFailureSchema.parse(JSON.parse(result.failureJson));
            throw new AgentExecutionFailure(failure, result.message);
          }
          const runMeta = parseRunMeta(result.metaJson);
          const requestFingerprints = collectRequestFingerprints(runMeta);
          const artifacts = result.ok
            ? z.array(runOutputArtifactSchema)
              .parse(JSON.parse(result.artifactsJson ?? "null"))
            : [];
          const observationRevision = await this.endObservation(session.logicalSessionRef, observation);
          observationEnded = true;
          for (const artifact of artifacts) {
            if (this.archive) {
              await this.archive.writeRunSnapshot({
                run_id: artifact.run_id, plan_commit_ref: input.runRef,
                logical_session_ref: session.logicalSessionRef, intent: session.intent,
                ok: result.ok, message: result.message,
                observation_revision: observationRevision,
                native_snapshot: artifact.snapshot,
                _meta: { plasm: { steps: runMeta?.steps ?? [], request_fingerprints: requestFingerprints } },
                archived_at: new Date().toISOString(),
              });
            }
            await this.materializeRunArtefact(artifact.run_id, artifact.snapshot, session.logicalSessionRef);
          }
          const runId = artifacts[0]?.run_id;
          await this.recordToolTrace("plasm", "plasm.live_run", started, {
            intent: session.intent,
            logical_session_ref: session.logicalSessionRef,
            plan_commit_ref: input.runRef,
            run_id: runId,
            catalog_cgs_hash: catalogCgsHash,
            ok: result.ok,
            trace_id: activeTraceId() ?? span.spanContext().traceId,
          });

          await this.emitHook("run:complete", {
            intent: session.intent,
            planCommitRef: input.runRef,
            runRef: input.runRef,
            runId,
            ok: result.ok,
            logicalSessionRef: session.logicalSessionRef,
          });

          if (result.ok) {
            const commit = session.planCommits.find((c) => c.ref === input.runRef);
            this.committedWriteOps += commit?.writeCount ?? 0;
          }

          const markdown = formatPlasmRunMarkdown(
            result.message,
            result.ok,
          );
          return markdown + `\n\n**Executed reference:** \`${input.runRef}\`` + formatRecordedOutputs(artifacts);
        } finally {
          if (!observationEnded) await this.endObservation(session.logicalSessionRef, observation);
        }
      },
    );
  }

  private async materializeRunArtefact(runId: string, payload: unknown, logicalSessionRef: string): Promise<void> {
    const safe = runId.replace(/[^a-zA-Z0-9_-]/g, "");
    if (!safe) return;
    await mkdir(this.artefactWorkspaceRoot, { recursive: true });
    const body = `${JSON.stringify(payload, null, 2)}\n`;
    await writeWorkspaceFile(this.artefactWorkspaceRoot, `artefacts/${logicalSessionRef}/${safe}.json`, body);
    await writeWorkspaceFile(this.artefactWorkspaceRoot, "artefacts/latest.json", body);
  }

  /** Machine evidence belongs to the existing trace archive, not model context. */
  async recordExecutionFailure(toolName: string, failure: ExecutionFailure): Promise<void> {
    await this.recordToolTrace("tool", toolName, Date.now(), { status: "execution_failed", failure_json: JSON.stringify(failure) });
  }

  private async emitHook(
    event: "plan:commit" | "run:complete",
    detail: Record<string, unknown>,
  ): Promise<void> {
    if (!this.hookRunner || !this.getAuthoringContext) return;
    await this.hookRunner.emit(event, this.getAuthoringContext(), detail);
  }

  private catalogHashForEntry(entryId: string): string | undefined {
    return this.loadedCatalogs.find((c) => c.manifest.entryId === entryId)?.manifest.cgsHash;
  }

  private async recordToolTrace(
    kind: string,
    name: string,
    startedMs: number,
    attributes: Record<string, string | number | boolean | undefined>,
  ): Promise<void> {
    if (!this.archive) return;
    const tenantId = this.sessionManager.tenant();
    const traceId = (attributes.trace_id as string | undefined) ?? randomUUID();
    const cleaned: Record<string, string | number | boolean> = {};
    for (const [key, value] of Object.entries(attributes)) {
      if (value !== undefined) cleaned[key] = value;
    }
    await this.archive.recordToolEvent(tenantId, traceId, kind, name, cleaned);
    await this.archive.finalizeTrace({
      summary: {
        trace_id: traceId,
        tenant_id: tenantId,
        logical_session_ref:
          typeof cleaned.logical_session_ref === "string"
            ? cleaned.logical_session_ref
            : undefined,
        intent: typeof cleaned.intent === "string" ? cleaned.intent : undefined,
        status: "completed",
        started_at_ms: startedMs,
        ended_at_ms: Date.now(),
        project_slug: "main",
        totals: { tool_calls: 1 },
      },
      records: [
        {
          at_ms: startedMs,
          kind,
          name,
          attributes: cleaned,
        },
      ],
    });
  }

  private sessionInputFailure(code: "invalid_context_input" | "invalid_logical_session_ref" | "unknown_logical_session_ref", diagnostic: string): AgentExecutionFailure {
    const open = this.workflowSession?.logicalSessionRef;
    const failure: ExecutionFailure = {
      cause: "program", recovery: "repair_program", code,
      diagnostic: `${diagnostic}. ${open ? `Open workflow logical_session_ref is \`${open}\`; copy it verbatim.` : 'Call plasm_context with session_mode "new" first, then copy its logical_session_ref verbatim.'}`,
      node: null, occurrence_path: [], catalog_digest: null,
      effects: [], dispatches: [], effects_unresolved: false,
    };
    return new AgentExecutionFailure(failure, `Execution stopped: ${failure.diagnostic}\nRevise the program and plan again.`);
  }

  private parseSessionRef(ref: string | undefined): string {
    const parsed = logicalSessionRefSchema.safeParse(ref);
    if (!parsed.success) {
      throw this.sessionInputFailure("invalid_logical_session_ref", "Expected a canonical logical_session_ref returned by plasm_context");
    }
    return parsed.data;
  }

  /** Missing/expired evidence is ordinary rejection; storage faults still propagate. */
  private async evidenceSession(ref: string): Promise<AgentSessionState | null> {
    const parsed = logicalSessionRefSchema.safeParse(ref);
    if (!parsed.success) return null;
    if (this.workflowSession?.logicalSessionRef === parsed.data) return this.workflowSession;
    const session = await this.sessionManager.getByLogicalRef(parsed.data);
    return session?.engineInstanceId === this.engineInstanceId ? session : null;
  }

  private async requireSessionByRef(ref: string): Promise<AgentSessionState> {
    const key = this.parseSessionRef(ref);
    if (this.workflowSession?.logicalSessionRef === key) {
      return this.workflowSession;
    }
    const session = await this.sessionManager.getByLogicalRef(key);
    if (!session) {
      throw this.sessionInputFailure("unknown_logical_session_ref", `Unknown logical_session_ref \`${key}\``);
    }
    if (session.engineInstanceId !== this.engineInstanceId) {
      throw new Error("Execution session expired with its native engine; explicitly open a new context");
    }
    this.workflowSession = session;
    return session;
  }
}
